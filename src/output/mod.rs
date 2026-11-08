pub mod sysml;

use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

pub fn write(files: &BTreeMap<String, String>, folder: &Path) -> Result<()> {
    for name in files.keys() {
        let path = Path::new(name);

        if path.extension().is_none_or(|ext| ext != "sysml")
            || name.contains('\\')
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("Invalid model path: {name}");
        }
    }

    if folder.exists() {
        for entry in WalkDir::new(folder).follow_links(false) {
            let entry = entry?;

            if entry.file_type().is_symlink()
                || !entry.file_type().is_dir()
                    && entry.path().extension().is_none_or(|ext| ext != "sysml")
            {
                bail!(
                    "Output must be a dedicated SysML folder: {}",
                    folder.display()
                );
            }
        }

        fs::remove_dir_all(folder)?;
    }

    for (name, text) in files {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().context("Model file has no parent")?)?;
        fs::write(&path, text).with_context(|| format!("Cannot write {}", path.display()))?;
    }

    Ok(())
}

pub fn check_no_overlap(input: &Path, output: &Path) -> Result<()> {
    let input = input.canonicalize()?;
    let output = absolute_path(output)?;

    if output.starts_with(&input) || input.starts_with(&output) {
        bail!("Input and output folders must not overlap");
    }

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;

    if path.exists() {
        return Ok(path.canonicalize()?);
    }

    let parent = path.parent().context("Output has no parent directory")?;
    let name = path.file_name().context("Invalid output path")?;
    Ok(absolute_path(parent)?.join(name))
}
