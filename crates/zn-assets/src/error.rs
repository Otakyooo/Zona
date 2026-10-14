use std::fmt;

#[derive(Debug)]
pub enum AssetError {
    Io(std::io::Error),
    Gltf(gltf::Error),
    Image(image::ImageError),
    UnsupportedFormat(String),
    Validation(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Gltf(e) => write!(f, "gltf error: {e}"),
            Self::Image(e) => write!(f, "image error: {e}"),
            Self::UnsupportedFormat(msg) => write!(f, "unsupported: {msg}"),
            Self::Validation(msg) => write!(f, "invalid asset: {msg}"),
        }
    }
}

impl std::error::Error for AssetError {}

impl From<std::io::Error> for AssetError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<gltf::Error> for AssetError {
    fn from(error: gltf::Error) -> Self {
        Self::Gltf(error)
    }
}

impl From<image::ImageError> for AssetError {
    fn from(error: image::ImageError) -> Self {
        Self::Image(error)
    }
}
