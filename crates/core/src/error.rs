use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

impl From<anyhow::Error> for CoreError {
    fn from(e: anyhow::Error) -> Self {
        CoreError::Msg(e.to_string())
    }
}
