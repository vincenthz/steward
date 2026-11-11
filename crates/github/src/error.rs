use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The request did not get an answer: DNS, TLS, connection, timeout
    Transport(String),
    /// GitHub answered with an error status
    Api {
        status: u16,
        message: String,
        /// when the rate limit is exhausted, the time it resets (unix seconds)
        rate_limit_reset: Option<u64>,
    },
    /// The answer is not what was expected
    Decode(String),
    /// The call needs a token and none was found
    NoToken,
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Api { status: 404, .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "github: {e}"),
            Error::Api {
                status,
                message,
                rate_limit_reset,
            } => {
                write!(f, "github: {status}: {message}")?;
                if let Some(reset) = rate_limit_reset {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    write!(
                        f,
                        " (rate limit exhausted, resets in {}s)",
                        reset.saturating_sub(now)
                    )?;
                }
                Ok(())
            }
            Error::Decode(e) => write!(f, "github: unexpected answer: {e}"),
            Error::NoToken => write!(
                f,
                "github: no token found (set GH_TOKEN, or log in with `gh auth login`)"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Json(e) => Error::Decode(e.to_string()),
            e => Error::Transport(e.to_string()),
        }
    }
}
