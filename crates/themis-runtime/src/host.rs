//! Host-command primitives: thin async wrappers around `ip`, `bridge`,
//! `iptables`, `sysctl`, and `tc`.
//!
//! Every function is async, instruments itself with `tracing`, and returns
//! [`themis_core::Result`].  The low-level helper [`run`] captures stdout,
//! stderr, and exit status; a non-zero exit is converted into
//! [`themis_core::Error::Runtime`] with the stderr text embedded.

use std::process::Output;

use regex::Regex;
use tokio::process::Command;
use tracing::{debug, instrument};

use themis_core::{Error, Result};

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

/// Run an external command, wait for it to finish, and return the captured
/// [`Output`].  A non-zero exit status is converted into
/// [`Error::Runtime`] with `stderr` included in the message.
async fn run(cmd: &str, args: &[&str]) -> Result<Output> {
    debug!(cmd = cmd, args = ?args, "executing host command");
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| {
            Error::Runtime(format!(
                "failed to spawn `{cmd}`: {e}"
            ))
        })?;

    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        Err(Error::Runtime(format!(
            "`{cmd} {}` exited with status {code}: {stderr}",
            args.join(" "),
        )))
    }
}

/// Like [`run`], but ignores a non-zero exit status — useful for "best
/// effort" teardown steps where a missing object is not an error.
async fn run_ignore_err(cmd: &str, args: &[&str]) -> Result<Output> {
    debug!(cmd = cmd, args = ?args, "executing host command (errors ignored)");
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| {
            Error::Runtime(format!(
                "failed to spawn `{cmd}`: {e}"
            ))
        })?;
    Ok(output)
}

// ---------------------------------------------------------------------------
// Bridge management
// ---------------------------------------------------------------------------

/// Create a Linux bridge with the given name.
///
/// Equivalent to `ip link add <name> type bridge`.
#[instrument(err)]
pub async fn create_bridge(name: &str) -> Result<()> {
    run("ip", &["link", "add", name, "type", "bridge"]).await?;
    debug!(bridge = name, "bridge created");
    Ok(())
}

/// Delete a Linux bridge by name.
///
/// Equivalent to `ip link delete <name> type bridge`.
#[instrument(err)]
pub async fn delete_bridge(name: &str) -> Result<()> {
    run("ip", &["link", "delete", name, "type", "bridge"]).await?;
    debug!(bridge = name, "bridge deleted");
    Ok(())
}

/// Return `true` when the named bridge interface already exists in the kernel.
///
/// Uses `ip link show <name>` and inspects the exit status; this avoids
/// parsing ambiguous output and mirrors what Ansible's `stat` module does.
#[instrument(err)]
pub async fn bridge_exists(name: &str) -> Result<bool> {
    let output = Command::new("ip")
        .args(["link", "show", name])
        .output()
        .await
        .map_err(|e| Error::Runtime(format!("failed to spawn `ip`: {e}")))?;
    let exists = output.status.success();
    debug!(bridge = name, exists, "bridge_exists check");
    Ok(exists)
}

// ---------------------------------------------------------------------------
// Link state
// ---------------------------------------------------------------------------

/// Bring a network interface up.
///
/// Equivalent to `ip link set <iface> up`.
#[instrument(err)]
pub async fn set_link_up(iface: &str) -> Result<()> {
    run("ip", &["link", "set", iface, "up"]).await?;
    debug!(iface, "link up");
    Ok(())
}

/// Take a network interface down.
///
/// Equivalent to `ip link set <iface> down`.
#[instrument(err)]
pub async fn set_link_down(iface: &str) -> Result<()> {
    run("ip", &["link", "set", iface, "down"]).await?;
    debug!(iface, "link down");
    Ok(())
}

// ---------------------------------------------------------------------------
// IP forwarding
// ---------------------------------------------------------------------------

/// Enable IPv4 forwarding kernel-wide via `sysctl`.
///
/// Equivalent to `sysctl -w net.ipv4.ip_forward=1`.
#[instrument(err)]
pub async fn enable_ip_forward() -> Result<()> {
    run("sysctl", &["-w", "net.ipv4.ip_forward=1"]).await?;
    debug!("IPv4 forwarding enabled");
    Ok(())
}

// ---------------------------------------------------------------------------
// iptables NAT / masquerade
// ---------------------------------------------------------------------------

/// Add an `iptables` MASQUERADE rule for the given source CIDR on the named
/// WAN interface.
///
/// Equivalent to:
/// ```text
/// iptables -t nat -A POSTROUTING -s <source_cidr> -o <wan_interface> -j MASQUERADE
/// ```
#[instrument(err)]
pub async fn add_masquerade(wan_interface: &str, source_cidr: &str) -> Result<()> {
    run(
        "iptables",
        &[
            "-t", "nat",
            "-A", "POSTROUTING",
            "-s", source_cidr,
            "-o", wan_interface,
            "-j", "MASQUERADE",
        ],
    )
    .await?;
    debug!(wan_interface, source_cidr, "masquerade rule added");
    Ok(())
}

/// Remove an `iptables` MASQUERADE rule that was previously added by
/// [`add_masquerade`].
///
/// Equivalent to the same command with `-D` instead of `-A`.
#[instrument(err)]
pub async fn remove_masquerade(wan_interface: &str, source_cidr: &str) -> Result<()> {
    run(
        "iptables",
        &[
            "-t", "nat",
            "-D", "POSTROUTING",
            "-s", source_cidr,
            "-o", wan_interface,
            "-j", "MASQUERADE",
        ],
    )
    .await?;
    debug!(wan_interface, source_cidr, "masquerade rule removed");
    Ok(())
}

// ---------------------------------------------------------------------------
// Bridge member enumeration
// ---------------------------------------------------------------------------

/// Return the names of every veth (or other) interface that is currently a
/// slave of the given bridge.
///
/// Parses `ip link show master <bridge>` output, extracting the interface
/// name at the start of each entry line.  The kernel prints:
///
/// ```text
/// 42: veth-a@veth-b: <BROADCAST,MULTICAST,UP,LOWER_UP> ...
/// ```
///
/// This function returns `["veth-a"]` (the part before any `@`).
#[instrument(err)]
pub async fn list_veths_on_bridge(bridge: &str) -> Result<Vec<String>> {
    let output = run("ip", &["link", "show", "master", bridge]).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let names = parse_link_show_output(&stdout);
    debug!(bridge, count = names.len(), "listed interfaces on bridge");
    Ok(names)
}

/// Parse the output of `ip link show master <bridge>` and return interface
/// names.
///
/// Matches lines beginning with an index number, e.g.:
/// ```text
/// 42: veth-a@veth-b: <...>
/// ```
/// Capturing `veth-a` (everything up to the first `@` or `:` after the
/// index+space prefix).
///
/// This is kept `pub(crate)` so unit tests can call it without a live kernel.
pub(crate) fn parse_link_show_output(output: &str) -> Vec<String> {
    // Pattern: beginning of line, digits, colon, space, then the interface
    // name which ends at the first `@` or `:`.
    let re = Regex::new(r"(?m)^\d+:\s+([^@:\s]+)").expect("static regex is valid");
    re.captures_iter(output)
        .map(|cap| cap[1].to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// tc / netem — chaos primitives
// ---------------------------------------------------------------------------

/// Apply a `netem` delay qdisc to an interface.
///
/// Removes any existing root qdisc first (ignoring errors), then runs:
/// ```text
/// tc qdisc add dev <iface> root netem delay <delay_ms>ms
/// ```
#[instrument(err)]
pub async fn apply_netem_delay(iface: &str, delay_ms: u32) -> Result<()> {
    // Best-effort removal of any existing qdisc.
    run_ignore_err("tc", &["qdisc", "del", "dev", iface, "root"]).await?;

    let delay_arg = format!("{delay_ms}ms");
    run(
        "tc",
        &["qdisc", "add", "dev", iface, "root", "netem", "delay", &delay_arg],
    )
    .await?;
    debug!(iface, delay_ms, "netem delay applied");
    Ok(())
}

/// Apply a `netem` packet-loss qdisc to an interface.
///
/// Removes any existing root qdisc first (ignoring errors), then runs:
/// ```text
/// tc qdisc add dev <iface> root netem loss <loss_pct>%
/// ```
#[instrument(err)]
pub async fn apply_netem_loss(iface: &str, loss_pct: u32) -> Result<()> {
    // Best-effort removal of any existing qdisc.
    run_ignore_err("tc", &["qdisc", "del", "dev", iface, "root"]).await?;

    let loss_arg = format!("{loss_pct}%");
    run(
        "tc",
        &["qdisc", "add", "dev", iface, "root", "netem", "loss", &loss_arg],
    )
    .await?;
    debug!(iface, loss_pct, "netem loss applied");
    Ok(())
}

/// Remove the root qdisc from an interface, clearing any `netem` impairments.
///
/// Equivalent to `tc qdisc del dev <iface> root`.
#[instrument(err)]
pub async fn remove_qdisc(iface: &str) -> Result<()> {
    run("tc", &["qdisc", "del", "dev", iface, "root"]).await?;
    debug!(iface, "root qdisc removed");
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — no live kernel required
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::parse_link_show_output;

    // Captured from a real `ip link show master br-lab0` on a running host.
    const SAMPLE_IP_LINK: &str = "\
42: veth-spine0a@veth-spine0b: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue master br-lab0 state UP mode DEFAULT group default qlen 1000
    link/ether aa:bb:cc:dd:ee:01 brd ff:ff:ff:ff:ff:ff
44: veth-leaf1a@veth-leaf1b: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue master br-lab0 state UP mode DEFAULT group default qlen 1000
    link/ether aa:bb:cc:dd:ee:02 brd ff:ff:ff:ff:ff:ff
";

    #[test]
    fn parse_two_veths() {
        let names = parse_link_show_output(SAMPLE_IP_LINK);
        assert_eq!(names, vec!["veth-spine0a", "veth-leaf1a"]);
    }

    #[test]
    fn parse_empty_output() {
        let names = parse_link_show_output("");
        assert!(names.is_empty(), "empty output should produce no names");
    }

    #[test]
    fn parse_single_veth_no_peer() {
        // Some interfaces don't have a peer (`@`) suffix.
        let input = "10: veth0: <BROADCAST,MULTICAST> mtu 1500\n";
        let names = parse_link_show_output(input);
        assert_eq!(names, vec!["veth0"]);
    }

    #[test]
    fn parse_ignores_continuation_lines() {
        // Lines that start with spaces (MAC address lines) must not be matched.
        let input = "\
7: veth-a@veth-b: <UP> mtu 1500\n    link/ether de:ad:be:ef:00:01 brd ff:ff:ff:ff:ff:ff\n";
        let names = parse_link_show_output(input);
        assert_eq!(names, vec!["veth-a"]);
    }

    #[test]
    fn parse_multi_digit_index() {
        let input = "1234: veth-border0@veth-border1: <UP> mtu 1500\n";
        let names = parse_link_show_output(input);
        assert_eq!(names, vec!["veth-border0"]);
    }

    #[test]
    fn parse_does_not_include_at_suffix() {
        let input = "5: eth0@if4: <UP> mtu 1500\n";
        let names = parse_link_show_output(input);
        assert_eq!(names[0], "eth0");
        assert!(!names[0].contains('@'));
    }

    // Verify that the run() helper error path builds a sensible message.
    // We exercise it by asking for a command that definitely does not exist.
    // (tokio::test is required because run() is async.)
    #[tokio::test]
    async fn run_spawn_failure_returns_runtime_error() {
        use super::run;
        use themis_core::Error;

        let result = run("__no_such_binary_themis__", &[]).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Runtime(msg) => {
                assert!(
                    msg.contains("__no_such_binary_themis__"),
                    "error message should contain the binary name: {msg}"
                );
            }
            other => panic!("expected Error::Runtime, got {other:?}"),
        }
    }

    // Verify that run_ignore_err() does NOT return Err on a non-zero exit.
    #[tokio::test]
    async fn run_ignore_err_does_not_fail_on_nonzero_exit() {
        use super::run_ignore_err;

        // `false` is a POSIX utility that always exits 1.
        let result = run_ignore_err("false", &[]).await;
        assert!(
            result.is_ok(),
            "run_ignore_err should succeed even on non-zero exit"
        );
    }
}
