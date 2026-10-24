//! One error type for the whole app.
//!
//! Every handler returns `Result<Response, AppError>`, so inside a handler you can use `?` on
//! anything that converts into `AppError` (see the `From` impls below) and the conversion to an
//! HTTP response happens in exactly one place: `AppError::into_response`.

use std::fmt;

use worker::Response;

#[derive(Debug)]
pub enum AppError {
    Unauthorized,
    NotFound,
    BadRequest(String),
    /// Anything that went wrong inside the Workers runtime (D1, fetch, ...).
    Internal(worker::Error),
}

impl AppError {
    pub fn status(&self) -> u16 {
        match self {
            AppError::Unauthorized => 401,
            AppError::NotFound => 404,
            AppError::BadRequest(_) => 400,
            AppError::Internal(_) => 500,
        }
    }

    pub fn into_response(self) -> worker::Result<Response> {
        if let AppError::Internal(err) = &self {
            worker::console_error!("internal error: {err}");
        }
        Response::error(self.to_string(), self.status())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Unauthorized => f.write_str("unauthorized"),
            AppError::NotFound => f.write_str("not found"),
            AppError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            // Don't leak internals to the client; the details go to the log instead.
            AppError::Internal(_) => f.write_str("internal error"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<worker::Error> for AppError {
    fn from(err: worker::Error) -> Self {
        AppError::Internal(err)
    }
}

impl From<url::ParseError> for AppError {
    fn from(err: url::ParseError) -> Self {
        AppError::BadRequest(format!("invalid url ({err})"))
    }
}

pub type AppResult<T> = Result<T, AppError>;
