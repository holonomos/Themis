//! Conversions between domain types (`themis-core`) and wire types
//! (`themis-proto`), plus the gRPC error mapping.

use crate::error::Error;

/// Map a `themis-core` error onto a gRPC `tonic::Status`. Preserves the
/// original error message and picks a reasonable gRPC code per variant.
impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        let msg = err.to_string();
        match err {
            // Bad user input / validation — client error.
            Error::InvalidParameter(_)
            | Error::Config(_)
            | Error::AddrParse(_)
            | Error::NetParse(_) => tonic::Status::invalid_argument(msg),

            // Resource not found — client error.
            Error::UnknownTemplate(_) | Error::UnknownPlatform(_) => {
                tonic::Status::not_found(msg)
            }

            // Everything else is a server-side problem.
            Error::Template(_)
            | Error::Platform(_)
            | Error::Runtime(_)
            | Error::Io(_)
            | Error::Json(_) => tonic::Status::internal(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn invalid_parameter_maps_to_invalid_argument() {
        let err = Error::InvalidParameter("oops".into());
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn unknown_template_maps_to_not_found() {
        let err = Error::UnknownTemplate("nope".into());
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), Code::NotFound);
    }

    #[test]
    fn runtime_error_maps_to_internal() {
        let err = Error::Runtime("boom".into());
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), Code::Internal);
    }

    #[test]
    fn error_message_is_preserved() {
        let err = Error::InvalidParameter("flag XYZ invalid".into());
        let status: tonic::Status = err.into();
        assert!(status.message().contains("flag XYZ invalid"));
    }
}
