def estimate(topology: dict, platform: dict) -> dict:
    """
    Calculate resource requirements.
    """
    vm_count = len(topology["nodes"])
    nominal_ram_mb = 0
    nominal_vcpu = 0
    
    for node in topology["nodes"].values():
        role = node["role"]
        prof = platform.get("resource_profiles", {}).get(role, {"vcpu": 1, "memory_mb": 256, "disk_gb": 3})
        node["vcpu"] = prof["vcpu"]
        node["memory_mb"] = prof["memory_mb"]
        node["disk_gb"] = prof["disk_gb"]
        nominal_ram_mb += prof["memory_mb"]
        nominal_vcpu += prof["vcpu"]
        
    base_mb = platform.get("ksm", {}).get("base_mb", 200)
    inc_mb = platform.get("ksm", {}).get("incremental_mb", 60)
    
    ksm_savings_mb = 0
    if vm_count > 0:
        ksm_savings_mb = (vm_count - 1) * max(0, base_mb - inc_mb)
        
    estimated_ram_mb = nominal_ram_mb - ksm_savings_mb
    
    # Check host available memory
    host_avail_mb = 16384 # fallback
    try:
        with open("/proc/meminfo", "r") as f:
            for line in f:
                if line.startswith("MemAvailable:"):
                    parts = line.split()
                    host_avail_mb = int(parts[1]) // 1024
                    break
    except Exception:
        pass
        
    fits = estimated_ram_mb < host_avail_mb
    
    return {
        "vm_count": vm_count,
        "nominal_ram_mb": nominal_ram_mb,
        "nominal_vcpu": nominal_vcpu,
        "ksm_savings_mb": ksm_savings_mb,
        "estimated_ram_mb": estimated_ram_mb,
        "estimated_ram_gb": round(estimated_ram_mb / 1024, 1),
        "host_available_mb": host_avail_mb,
        "fits": fits
    }

def print_estimate(estimate_info: dict) -> None:
    """Print a human-readable estimate table to stdout."""
    print("--- Resource Estimate ---")
    print(f"VM Count:          {estimate_info['vm_count']}")
    print(f"Nominal vCPUs:     {estimate_info['nominal_vcpu']}")
    print(f"Nominal RAM:       {estimate_info['nominal_ram_mb']} MB")
    print(f"KSM Savings:       {estimate_info['ksm_savings_mb']} MB")
    print(f"Estimated RAM:     {estimate_info['estimated_ram_mb']} MB ({estimate_info['estimated_ram_gb']} GB)")
    print(f"Host Available:    {estimate_info['host_available_mb']} MB")
    if estimate_info['fits']:
        print("Status:            [OK] Fits in host memory.")
    else:
        print("Status:            [WARNING] Exceeds available host memory!")
