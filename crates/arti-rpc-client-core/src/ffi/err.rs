//! Error handling logic for our ffi code.

use c_str_macro::c_str;
use paste::paste;
use std::ffi::{c_char, CStr};
use std::fmt::Display;
use std::panic::{catch_unwind, UnwindSafe};

use crate::conn::ErrorResponse;
use crate::util::Utf8CString;

use super::util::{ffi_body_raw, OptOutPtrExt as _, OutPtr};
use super::ArtiRpcStatus;

/// Helper:
/// Given a restricted enum defining FfiStatus, also define a series of constants for its variants,
/// and a string conversion function.

// NOTE: I tried to use derive_deftly here, but ran into trouble when defining the constants.
// I wanted to have them be "pub const ARTI_FOO = FfiStatus::$vname",
// but that doesn't work with cbindgen, which won't expose a constant unless it is a public type
// it can recognize.
// There is no way to use derive_deftly to look at the explicit discriminant of an enum.
macro_rules! define_ffi_status {
    {
        $(#[$tm:meta])*
        pub(crate) enum FfiStatus {
            $(
                $(#[$m:meta])*
                [$s:expr]
                $id:ident = $e:expr,
            )+
        }

    } => {paste!{
        $(#[$tm])*
        pub(crate) enum FfiStatus {
            $(
                $(#[$m])*
                $id = $e,
            )+
        }

        $(
            $(#[$m])*
            pub const [<ARTI_RPC_STATUS_ $id:snake:upper >] : ArtiRpcStatus = $e;
        )+

        /// Return a string representing the meaning of a given `ArtiRpcStatus`.
        ///
        /// The result will always be non-NULL, even if the status is unrecognized.
        #[no_mangle]
        pub extern "C" fn arti_status_to_str(status: ArtiRpcStatus) -> *const c_char {
            match status {
                $(
                    [<ARTI_RPC_STATUS_ $id:snake:upper>] => c_str!($s),
                )+
                _ => c_str!("(unrecognized status)"),
            }.as_ptr()
        }
    }}
}

define_ffi_status! {
/// View of FFI status as rust enumeration.
///
/// Not exposed in the FFI interfaces, except via cast to ArtiStatus.
///
/// We define this as an enumeration so that we can treat it exhaustively in Rust.
#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub(crate) enum FfiStatus {
    /// The function has returned successfully.
    #[allow(dead_code)]
    ["Success"]
    Success = 0,

    /// One or more of the inputs to a library function was invalid.
    ///
    /// (This error was generated by the library, before any request was sent.)
    ["Invalid input"]
    InvalidInput = 1,

    /// Tried to use some functionality
    /// (for example, an authentication method or connection scheme)
    /// that wasn't available on this platform or build.
    ///
    /// (This error was generated by the library, before any request was sent.)
    ["Not supported"]
    NotSupported = 2,

    /// Tried to connect to Arti, but an IO error occurred.
    ///
    /// This may indicate that Arti wasn't running,
    /// or that Arti was built without RPC support,
    /// or that Arti wasn't running at the specified location.
    ///
    /// (This error was generated by the library.)
    ["An IO error ocurred while connecting to Arti"]
    ConnectIo = 3,

    /// We tried to authenticate with Arti, but it rejected our attempt.
    ///
    /// (This error was sent by the peer.)
    ["Authentication rejected"]
    BadAuth = 4,

    /// Our peer has, in some way, violated the Arti-RPC protocol.
    ///
    /// (This error was generated by the library,
    /// based on a response from Arti that appeared to be invalid.)
    ["Peer violated the RPC protocol"]
    PeerProtocolViolation = 5,

    /// The peer has closed our connection; possibly because it is shutting down.
    ///
    /// (This error was generated by the library,
    /// based on the connection being closed or reset from the peer.)
    ["Peer has shut down"]
    Shutdown = 6,

    /// An internal error occurred in the arti rpc client.
    ///
    /// (This error was generated by the library.
    /// If you see it, there is probably a bug in the library.)
    ["Internal error; possible bug?"]
    Internal = 7,

    /// The peer reports that one of our requests has failed.
    ///
    /// (This error was sent by the peer, in response to one of our requests.
    /// No further responses to that request will be received or accepted.)
    ["Request has failed"]
    RequestFailed = 8,

    /// Tried to check the status of a request and found that it was no longer running.
    ///
    /// TODO RPC: We should make sure that this is the actual semantics we want for this
    /// error!  Revisit after we have implemented real cancellation.
    ["Request was cancelled"]
    RequestCancelled = 9,
}
}

/// An error as returned by the Arti FFI code.
#[derive(Debug, Clone)]
pub struct FfiError {
    /// The status of this error messages
    pub(super) status: ArtiRpcStatus,
    /// A human-readable message describing this error
    message: Utf8CString,
    /// If present, a Json-formatted message from our peer that we are representing with this error.
    error_response: Option<ErrorResponse>,
}

impl FfiError {
    /// Helper: If this error stems from a resoponse from our RPC peer,
    /// return that reponse.
    fn error_response_as_ptr(&self) -> Option<*const c_char> {
        self.error_response.as_ref().map(|response| {
            let cstr: &CStr = response.as_ref();
            cstr.as_ptr()
        })
    }
}

/// Convenience trait to help implement `Into<FfiError>`
///
/// Any error that implements this trait will be convertible into an [`FfiError`].
// additional requirements: display doesn't make NULs.
pub(crate) trait IntoFfiError: Display + Sized {
    /// Return the status
    fn status(&self) -> FfiStatus;
    /// Return a message for this error.
    ///
    /// By default, returns the Display of this error.
    fn message(&self) -> String {
        self.to_string()
    }
    /// Consume this error and return an [`ErrorResponse`]
    fn into_error_response(self) -> Option<ErrorResponse> {
        None
    }
}
impl<T: IntoFfiError> From<T> for FfiError {
    fn from(value: T) -> Self {
        let status = value.status() as u32;
        let message = value
            .message()
            .try_into()
            .expect("Error message had a NUL?");
        let error_response = value.into_error_response();
        Self {
            status,
            message,
            error_response,
        }
    }
}
impl From<void::Void> for FfiError {
    fn from(value: void::Void) -> Self {
        void::unreachable(value)
    }
}

/// Tried to call a ffi function with a not-permitted argument.
#[derive(Clone, Debug, thiserror::Error)]
pub(super) enum InvalidInput {
    /// Tried to convert a NULL pointer to a string.
    #[error("Provided string was NULL.")]
    NullPointer,

    /// Tried to convert a non-UTF string.
    #[error("Provided string was not UTF-8")]
    BadUtf8,
}

impl From<void::Void> for InvalidInput {
    fn from(value: void::Void) -> Self {
        void::unreachable(value)
    }
}

impl IntoFfiError for InvalidInput {
    fn status(&self) -> FfiStatus {
        FfiStatus::InvalidInput
    }
}

impl IntoFfiError for crate::ConnectError {
    fn status(&self) -> FfiStatus {
        use crate::ConnectError as E;
        use FfiStatus as F;
        match self {
            E::SchemeNotSupported => F::NotSupported,
            E::CannotConnect(_) => F::ConnectIo,
            E::AuthenticationRejected(_) => F::BadAuth,
            E::BadMessage(_) => F::PeerProtocolViolation,
            E::ProtoError(e) => e.status(),
        }
    }

    fn into_error_response(self) -> Option<ErrorResponse> {
        use crate::ConnectError as E;
        match self {
            E::AuthenticationRejected(msg) => Some(msg),
            _ => None,
        }
    }
}

impl IntoFfiError for crate::ProtoError {
    fn status(&self) -> FfiStatus {
        use crate::ProtoError as E;
        use FfiStatus as F;
        match self {
            E::Shutdown(_) => F::Shutdown,
            E::InvalidRequest(_) => F::InvalidInput,
            E::RequestIdInUse => F::InvalidInput,
            E::RequestCancelled => F::RequestCancelled,
            E::DuplicateWait => F::Internal,
            E::CouldNotEncode(_) => F::Internal,
        }
    }
}

impl IntoFfiError for crate::BuilderError {
    fn status(&self) -> FfiStatus {
        use crate::BuilderError as E;
        use FfiStatus as F;
        match self {
            E::InvalidConnectString => F::InvalidInput,
        }
    }
}

impl IntoFfiError for ErrorResponse {
    fn status(&self) -> FfiStatus {
        FfiStatus::RequestFailed
    }
    fn into_error_response(self) -> Option<ErrorResponse> {
        Some(self)
    }
}

/// An error returned by the Arti RPC code, exposed as an object.
///
/// When a function returns an [`ArtiRpcStatus`] other than [`ARTI_RPC_STATUS_SUCCESS`],
/// it will also expose a newly allocated value of this type
/// via its `error_out` parameter.
pub type ArtiRpcError = FfiError;

/// Return the status code associated with a given error.
///
/// If `err` is NULL, return [`ARTI_RPC_STATUS_INVALID_INPUT`].
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn arti_rpc_err_status(err: *const ArtiRpcError) -> ArtiRpcStatus {
    ffi_body_raw!(
        {
            let err: Option<&ArtiRpcError> [in_ptr_opt];
        } in {
            err.map(|e| e.status)
               .unwrap_or(ARTI_RPC_STATUS_INVALID_INPUT)
            // Safety: Return value is ArtiRpcStatus; trivially safe.
        }
    )
}

/// Return a human-readable error message associated with a given error.
///
/// The format of these messages may change arbitrarily between versions of this library;
/// it is a mistake to depend on the actual contents of this message.
///
/// Return NULL if the input `err` is NULL.
///
/// # Correctness requirements
///
/// The resulting string pointer is valid only for as long as the input `err` is not freed.
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn arti_rpc_err_message(err: *const ArtiRpcError) -> *const c_char {
    ffi_body_raw!(
        {
            let err: Option<&ArtiRpcError> [in_ptr_opt];
        } in {
            err.map(|e| e.message.as_ptr())
               .unwrap_or(std::ptr::null())
            // Safety: returned pointer is null, or semantically borrowed from `err`.
            // It is only null if `err` was null.
            // The caller is not allowed to modify it.
        }
    )
}

/// Return a Json-formatted error response associated with a given error.
///
/// These messages are full responses, including the `error` field,
/// and the `id` field (if present).
///
/// Return NULL if the specified error does not represent an RPC error response.
///
/// Return NULL if the input `err` is NULL.
///
/// # Correctness requirements
///
/// The resulting string pointer is valid only for as long as the input `err` is not freed.
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn arti_rpc_err_response(err: *const ArtiRpcError) -> *const c_char {
    ffi_body_raw!(
        {
            let err: Option<&ArtiRpcError> [in_ptr_opt];
        } in {
            err.and_then(ArtiRpcError::error_response_as_ptr)
               .unwrap_or(std::ptr::null())
            // Safety: returned pointer is null, or semantically borrowed from `err`.
            // It is only null if `err` was null, or if `err` contained no response field.
            // The caller is not allowed to modify it.
        }
    )
}

/// Make and return copy of a provided error.
///
/// Return NULL if the input is NULL.
///
/// # Ownership
///
/// The caller is responsible for making sure that the returned object
/// is eventually freed with `arti_rpc_err_free()`.
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn arti_rpc_err_clone(err: *const ArtiRpcError) -> *mut ArtiRpcError {
    ffi_body_raw!(
        {
            let err: Option<&ArtiRpcError> [in_ptr_opt];
        } in {
            err.map(|e| Box::into_raw(Box::new(e.clone())))
               .unwrap_or(std::ptr::null_mut())
            // Safety: returned pointer is null, or newly allocated via Box::new().
            // It is only null if the input was null.
        }
    )
}

/// Release storage held by a provided error.
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn arti_rpc_err_free(err: *mut ArtiRpcError) {
    ffi_body_raw!(
        {
            let err: Option<Box<ArtiRpcError>> [in_ptr_consume_opt];
            // Safety: Return value is (); trivially safe.
        } in {
            drop(err);
        }
    );
}

/// Run `body` and catch panics.  If one occurs, return the result of `on_err` instead.
///
/// We wrap the body of every C ffi function with this function
/// (or with `handle_errors`, which uses this function),
/// even if we do not think that the body can actually panic.
pub(super) fn abort_on_panic<F, T>(body: F) -> T
where
    F: FnOnce() -> T + UnwindSafe,
{
    #[allow(clippy::print_stderr)]
    match catch_unwind(body) {
        Ok(x) => x,
        Err(_panic_info) => {
            eprintln!("Internal panic in arti-rpc library: aborting!");
            std::process::abort();
        }
    }
}

/// Call `body`, converting any errors or panics that occur into an FfiError,
/// and storing that error in `error_out`.
pub(super) fn handle_errors<F>(error_out: Option<OutPtr<FfiError>>, body: F) -> ArtiRpcStatus
where
    F: FnOnce() -> Result<(), FfiError> + UnwindSafe,
{
    match abort_on_panic(body) {
        Ok(()) => ARTI_RPC_STATUS_SUCCESS,
        Err(e) => {
            // "body" returned an error.
            let status = e.status;
            error_out.write_value_if_ptr_set(e);
            status
        }
    }
}
