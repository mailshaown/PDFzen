//! PDF composition engine shared by the app and by tests/examples.

mod pdf;
mod security;

pub use pdf::{
    Anno, DocInput, ExtractedImage, PageSpec, Payload, blank, build, extract_images, images_to_pdf,
};
pub use security::{OpenOutcome, SealStatus, SecurityInput, open_pdf_bytes};
