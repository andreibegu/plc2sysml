mod behavior;
mod expressions;
mod program;
mod validate;

pub use behavior::*;
pub use expressions::*;
pub use program::*;
pub use validate::validate;
pub(crate) use validate::validate_graph;

use sha2::{Digest, Sha256};

pub fn id(parts: &[&str]) -> String {
    let mut hash = Sha256::new();

    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }

    format!("e_{:x}", hash.finalize())
}
