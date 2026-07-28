use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::update_fs::{validate_regular_directory, validate_regular_file};

pub const PACKAGE_MANIFEST_NAME: &str = "release-package.json";
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;
pub const PRODUCT_ID: &str = "codex-web-terminal";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const VITE_ASSET_PREFIX: &str = "/assets/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleasePackageManifest {
    pub schema_version: u32,
    pub product: String,
    pub version: String,
    pub target: String,
}

impl ReleasePackageManifest {
    pub fn load(package_root: &Path) -> Result<Self> {
        validate_regular_directory(package_root)
            .context("release package directory is not a regular directory")?;
        let manifest_path = package_root.join(PACKAGE_MANIFEST_NAME);
        validate_regular_file(&manifest_path, Some(MAX_MANIFEST_BYTES)).with_context(|| {
            format!(
                "official release marker is missing or unsafe: {}",
                manifest_path.display()
            )
        })?;

        let bytes = fs::read(&manifest_path).with_context(|| {
            format!(
                "failed to read official release marker: {}",
                manifest_path.display()
            )
        })?;
        let manifest: Self =
            serde_json::from_slice(&bytes).context("official release marker is invalid")?;
        Ok(manifest)
    }

    pub fn validate(&self, expected_version: &Version, expected_target: &str) -> Result<()> {
        if self.schema_version != PACKAGE_SCHEMA_VERSION {
            bail!(
                "unsupported release marker schema version: {}",
                self.schema_version
            );
        }
        if self.product != PRODUCT_ID {
            bail!("release marker identifies a different product");
        }
        let version =
            Version::parse(&self.version).context("release marker contains an invalid version")?;
        if &version != expected_version {
            bail!(
                "release marker version {} does not match expected version {}",
                version,
                expected_version
            );
        }
        if self.target != expected_target {
            bail!(
                "release marker target {} does not match this platform {}",
                self.target,
                expected_target
            );
        }
        Ok(())
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub fn current_release_target() -> Result<&'static str> {
    Ok("x86_64-pc-windows-msvc")
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn current_release_target() -> Result<&'static str> {
    Ok("x86_64-unknown-linux-gnu")
}

#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
pub fn current_release_target() -> Result<&'static str> {
    bail!("automatic updates are not available for this operating system and architecture")
}

pub const fn executable_name() -> &'static str {
    if cfg!(windows) {
        "codex-web.exe"
    } else {
        "codex-web"
    }
}

pub fn expected_archive_name(version: &Version) -> Result<String> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Ok(format!("codex-web-terminal-v{version}-windows-x86_64.zip"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(format!(
            "codex-web-terminal-v{version}-linux-x86_64-glibc.tar.gz"
        ))
    } else {
        bail!("automatic updates are not available for this platform")
    }
}

pub fn expected_archive_root(version: &Version) -> Result<String> {
    expected_archive_name(version).map(|name| {
        name.strip_suffix(".tar.gz")
            .or_else(|| name.strip_suffix(".zip"))
            .expect("supported archive names have a known extension")
            .to_owned()
    })
}

pub fn package_root_for_executable(executable: &Path) -> Result<PathBuf> {
    executable
        .parent()
        .map(Path::to_path_buf)
        .context("the executable has no parent directory")
}

pub fn validate_package_layout(
    package_root: &Path,
    expected_version: &Version,
    expected_target: &str,
) -> Result<ReleasePackageManifest> {
    let manifest = ReleasePackageManifest::load(package_root)?;
    manifest.validate(expected_version, expected_target)?;

    for relative in ["web", "THIRD_PARTY_LICENSES"] {
        validate_regular_directory(&package_root.join(relative)).with_context(|| {
            format!("release package directory is missing or unsafe: {relative}")
        })?;
    }

    for relative in [
        executable_name(),
        "README.md",
        "BUILDING.md",
        "OPERATIONS.md",
        "SECURITY.md",
        "LICENSE",
        "THIRD_PARTY_LICENSES/THIRD_PARTY_LICENSES.txt",
        "THIRD_PARTY_LICENSES/manifest.json",
    ] {
        let path = package_root.join(relative);
        validate_regular_file(&path, None)
            .with_context(|| format!("release package is missing {}", path.display()))?;
    }
    validate_frontend_assets(package_root)?;

    Ok(manifest)
}

fn validate_frontend_assets(package_root: &Path) -> Result<()> {
    let web_root = package_root.join("web");
    let index_path = web_root.join("index.html");
    validate_regular_file(&index_path, Some(MAX_INDEX_BYTES))
        .context("release package web/index.html is missing, unsafe, or too large")?;
    let bytes = fs::read(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    let index = std::str::from_utf8(&bytes).context("release package index.html is not UTF-8")?;

    let mut assets = HashSet::new();
    let mut has_javascript = false;
    let mut has_stylesheet = false;
    for attribute in ["src", "href"] {
        visit_quoted_attributes(index, attribute, |value| {
            let looks_like_code = value.ends_with(".js") || value.ends_with(".css");
            let Some(asset_name) = value.strip_prefix(VITE_ASSET_PREFIX) else {
                if looks_like_code {
                    bail!("release package index.html references unpackaged code");
                }
                return Ok(());
            };
            validate_vite_asset_name(asset_name)?;
            has_javascript |= asset_name.ends_with(".js");
            has_stylesheet |= asset_name.ends_with(".css");
            assets.insert(asset_name.to_owned());
            Ok(())
        })?;
    }
    if !has_javascript || !has_stylesheet {
        bail!("release package index.html must reference hashed Vite JavaScript and CSS assets");
    }

    for asset in assets {
        let path = web_root.join("assets").join(&asset);
        validate_regular_file(&path, None).with_context(|| {
            format!(
                "release package index.html references a missing or unsafe asset: {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn visit_quoted_attributes(
    document: &str,
    attribute: &str,
    mut visitor: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let bytes = document.as_bytes();
    let name = attribute.as_bytes();
    let mut offset = 0;
    while offset + name.len() <= bytes.len() {
        if &bytes[offset..offset + name.len()] != name
            || (offset > 0 && bytes[offset - 1] != b'<' && !bytes[offset - 1].is_ascii_whitespace())
        {
            offset += 1;
            continue;
        }

        let mut cursor = offset + name.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            offset += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || !matches!(bytes[cursor], b'\'' | b'"') {
            bail!("release package index.html contains an unquoted {attribute} attribute");
        }
        let quote = bytes[cursor];
        let value_start = cursor + 1;
        let Some(value_length) = bytes[value_start..]
            .iter()
            .position(|candidate| *candidate == quote)
        else {
            bail!("release package index.html contains an unterminated {attribute} attribute");
        };
        let value_end = value_start + value_length;
        visitor(&document[value_start..value_end])?;
        offset = value_end + 1;
    }
    Ok(())
}

fn validate_vite_asset_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("release package index.html contains an unsafe Vite asset name");
    }
    let (stem, extension) = name
        .rsplit_once('.')
        .context("release package Vite asset has no extension")?;
    if !matches!(extension, "js" | "css")
        || !stem
            .match_indices('-')
            .any(|(index, _)| is_vite_hash(&stem[index + 1..]))
    {
        bail!("release package index.html references a non-hashed Vite asset");
    }
    Ok(())
}

fn is_vite_hash(value: &str) -> bool {
    (8..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub fn is_official_current_package(executable: &Path) -> Result<()> {
    let target = current_release_target()?;
    let version =
        Version::parse(env!("CARGO_PKG_VERSION")).context("the build version is invalid")?;
    let package_root = package_root_for_executable(executable)?;
    validate_package_layout(&package_root, &version, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_identity_matches_the_current_platform() {
        let version = Version::new(1, 2, 3);
        let name = expected_archive_name(&version).expect("supported test platform");
        let root = expected_archive_root(&version).expect("supported test platform");

        assert!(name.starts_with("codex-web-terminal-v1.2.3-"));
        assert_eq!(
            root,
            name.trim_end_matches(".zip").trim_end_matches(".tar.gz")
        );
    }

    #[test]
    fn manifest_rejects_a_different_product_or_version() {
        let expected = Version::new(1, 2, 3);
        let target = current_release_target().expect("supported test platform");
        let valid = ReleasePackageManifest {
            schema_version: PACKAGE_SCHEMA_VERSION,
            product: PRODUCT_ID.to_owned(),
            version: expected.to_string(),
            target: target.to_owned(),
        };
        valid.validate(&expected, target).expect("valid manifest");

        let mut wrong_product = valid.clone();
        wrong_product.product = "another-product".to_owned();
        assert!(wrong_product.validate(&expected, target).is_err());

        let mut wrong_version = valid;
        wrong_version.version = "1.2.4".to_owned();
        assert!(wrong_version.validate(&expected, target).is_err());
    }

    #[test]
    fn frontend_layout_requires_real_referenced_vite_assets() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let package = temporary.path();
        let assets = package.join("web").join("assets");
        fs::create_dir_all(&assets).expect("assets directory");
        fs::write(
            package.join("web").join("index.html"),
            concat!(
                "<!doctype html><script type=\"module\" ",
                "src=\"/assets/index-Cq2D_58X.js\"></script>",
                "<link rel=\"stylesheet\" href='/assets/index-By1AeVe3.css'>"
            ),
        )
        .expect("index fixture");
        fs::write(assets.join("index-Cq2D_58X.js"), b"export {};\n").expect("JavaScript fixture");
        fs::write(assets.join("index-By1AeVe3.css"), b"body{}\n").expect("CSS fixture");

        validate_frontend_assets(package).expect("complete Vite frontend");
        fs::remove_file(assets.join("index-Cq2D_58X.js")).expect("remove JavaScript fixture");
        assert!(validate_frontend_assets(package).is_err());
    }

    #[test]
    fn frontend_layout_rejects_unbounded_invalid_or_unhashed_indexes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let web = temporary.path().join("web");
        fs::create_dir_all(web.join("assets")).expect("assets directory");
        let index = web.join("index.html");

        fs::write(&index, [0xff]).expect("invalid UTF-8 fixture");
        assert!(validate_frontend_assets(temporary.path()).is_err());

        fs::write(
            &index,
            "<script src=\"/assets/index.js\"></script>\
             <link href=\"/assets/index.css\" rel=\"stylesheet\">",
        )
        .expect("unhashed fixture");
        assert!(validate_frontend_assets(temporary.path()).is_err());

        fs::write(&index, vec![b'x'; MAX_INDEX_BYTES as usize + 1]).expect("oversized fixture");
        assert!(validate_frontend_assets(temporary.path()).is_err());
    }
}
