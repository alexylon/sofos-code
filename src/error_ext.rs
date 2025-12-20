use crate::error::{Result, SofosError};

pub trait ResultExt<T> {
    fn context(self, msg: impl Into<String>) -> Result<T>;
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T, E> ResultExt<T> for std::result::Result<T, E>
where
    E: Into<SofosError>,
{
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.map_err(|e| {
            let base = e.into();
            SofosError::Context {
                message: msg.into(),
                source: Box::new(base),
            }
        })
    }

    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|e| {
            let base = e.into();
            SofosError::Context {
                message: f(),
                source: Box::new(base),
            }
        })
    }
}

impl<T> ResultExt<T> for Option<T> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| SofosError::Config(msg.into()))
    }

    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.ok_or_else(|| SofosError::Config(f()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_result_context() {
        let result: std::io::Result<()> = Err(io::Error::new(io::ErrorKind::NotFound, "test"));
        let err = result.context("Failed to read config").unwrap_err();

        assert!(matches!(err, SofosError::Context { .. }));
    }

    #[test]
    fn test_option_context() {
        let opt: Option<i32> = None;
        let err = opt.context("Value not found").unwrap_err();

        assert!(matches!(err, SofosError::Config(_)));
    }

    #[test]
    fn test_with_context() {
        let result: std::io::Result<()> = Err(io::Error::new(io::ErrorKind::NotFound, "test"));
        let err = result
            .with_context(|| format!("Failed to read file: test.txt"))
            .unwrap_err();

        assert!(matches!(err, SofosError::Context { .. }));
    }
}
