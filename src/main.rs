//! Static site generator for `wdl-lint`/`wdl-analysis` lints.

use std::ffi::OsStr;
use std::fmt::Write;
use std::io::BufRead;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::bail;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tracing::level_filters::LevelFilter;
use walkdir::WalkDir;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    if let Err(e) = real_main() {
        tracing::error!("failed to generate wdl-lint site: {e}");
        std::process::exit(1);
    }
}

fn sprocket_repo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sprocket")
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LintSource {
    #[serde(rename = "wdl-lint")]
    WdlLint,
    #[serde(rename = "wdl-analysis")]
    WdlAnalysis,
}

#[derive(Debug, Deserialize)]
pub struct ConfigField {
    /// The name of the field.
    pub name: String,
    /// The description of the config field.
    pub description: String,
    /// The default value of the field as a TOML string.
    pub default: String,
}

#[derive(Debug, Deserialize)]
pub struct Tag {
    pub name: String,
    pub applicable_lints: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    pub source: LintSource,
    pub id: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub description: String,
    pub explanation: String,
    pub examples: Vec<Example>,
    pub url: Option<String>,
    #[serde(default)]
    pub related: Option<Vec<String>>,
    #[serde(default)]
    pub config: Option<Vec<ConfigField>>,
}

#[derive(Debug, Deserialize)]
pub struct Example {
    negative: LabeledSnippet,
    revised: Option<LabeledSnippet>,
}

#[derive(Debug, Deserialize)]
pub struct LabeledSnippet {
    pub label: Option<String>,
    pub snippet: String,
}

impl Rule {
    /// Render the rule's Markdown documentation as HTML.
    fn render(&self) -> String {
        let mut markdown = format!(
            r#"### What it Does
{description}

### Why is this Bad?
{explanation}
"#,
            description = self.description,
            explanation = self.explanation
        );

        let examples = &self.examples;
        if !examples.is_empty() {
            writeln!(&mut markdown).unwrap();
            writeln!(&mut markdown, "### Examples").unwrap();
            for example in examples {
                if let Some(label) = &example.negative.label {
                    writeln!(&mut markdown, "{label}:").unwrap();
                }

                writeln!(&mut markdown, "```wdl\n{}```", example.negative.snippet).unwrap();

                if let Some(revised) = &example.revised {
                    writeln!(
                        &mut markdown,
                        "{}:",
                        revised.label.as_deref().unwrap_or("Use instead")
                    )
                    .unwrap();
                    writeln!(&mut markdown, "```wdl\n{}```", revised.snippet).unwrap();
                }
            }
            writeln!(&mut markdown).unwrap();
        }

        if let Some(config_fields) = self.config.as_ref().filter(|f| !f.is_empty()) {
            writeln!(&mut markdown, "<div class=\"rule-configuration\">").unwrap();
            writeln!(&mut markdown).unwrap();
            writeln!(&mut markdown, "### Configuration").unwrap();
            for field in config_fields {
                writeln!(
                    &mut markdown,
                    "`{}` (Default: `{}`)",
                    field.name, field.default
                )
                .unwrap();
                writeln!(&mut markdown).unwrap();
                writeln!(&mut markdown, "{}", field.description).unwrap();
            }
            writeln!(&mut markdown, "</div>").unwrap();
        }

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_GFM);
        options.insert(Options::ENABLE_DEFINITION_LIST);
        let parser = Parser::new_ext(&markdown, options);

        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, parser);

        html
    }

    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            String::from("source"),
            serde_json::to_value(self.source).unwrap(),
        );
        obj.insert(String::from("id"), serde_json::to_value(&self.id).unwrap());
        if let Some(tags) = &self.tags {
            obj.insert(String::from("tags"), serde_json::to_value(tags).unwrap());
        }
        obj.insert(
            String::from("description"),
            serde_json::to_value(&self.description).unwrap(),
        );
        obj.insert(
            String::from("descriptionHtml"),
            serde_json::to_value(self.render()).unwrap(),
        );

        Value::Object(obj)
    }
}

fn all_tags(sprocket_dir: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new("cargo")
        .args([
            "run",
            "-p",
            "sprocket",
            "--bin",
            "sprocket",
            "--",
            "explain",
            "--list-all-tags",
            "--format=json",
        ])
        .current_dir(sprocket_dir)
        .output()?;
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        bail!("failed to run sprocket");
    }

    match serde_json::from_slice::<Vec<Tag>>(&output.stdout) {
        Ok(tags) => Ok(tags.into_iter().map(|t| t.name).collect()),
        Err(e) => Err(e.into()),
    }
}

fn all_rules(sprocket_dir: &Path) -> anyhow::Result<Vec<Rule>> {
    let output = Command::new("cargo")
        .args([
            "run",
            "-p",
            "sprocket",
            "--bin",
            "sprocket",
            "--",
            "explain",
            "--list-all-rules",
            "--format=json",
        ])
        .current_dir(sprocket_dir)
        .output()?;
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        bail!("failed to run sprocket");
    }

    serde_json::from_slice::<Vec<Rule>>(&output.stdout).map_err(Into::into)
}

fn latest_version_of(crate_name: &str, sprocket_dir: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["tag", "-l"])
        .arg(format!("{crate_name}-*"))
        .arg("--sort=-version:refname")
        .current_dir(sprocket_dir)
        .output()?;
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        bail!("failed to determine latest version of `{crate_name}`");
    }

    let tag = output
        .stdout
        .lines()
        .next()
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("no versions of `{crate_name}` found"))?;

    Ok(tag
        .strip_prefix(&format!("{crate_name}-"))
        .map(ToString::to_string)
        .expect("tag should contain name"))
}

/// The main program logic.
fn real_main() -> anyhow::Result<()> {
    let sprocket_dir = sprocket_repo_dir();
    if !sprocket_dir.exists() {
        bail!("expected sprocket repo at '{}'", sprocket_dir.display());
    }

    let dist_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist");
    if dist_dir.exists() {
        std::fs::remove_dir_all(&dist_dir)?;
    }

    std::fs::create_dir(&dist_dir)?;

    dump_default_state_json(&sprocket_dir)?;

    tracing::info!("copying static files to `{}`", dist_dir.display());
    copy_files_to_dist(&dist_dir, &sprocket_dir)?;
    compile_external(&sprocket_dir)?;

    Ok(())
}

/// Gets the `web-common` dir at the root of the project.
fn web_common_dir(sprocket_dir: &Path) -> std::io::Result<PathBuf> {
    let web_common_dir = sprocket_dir.join("web-common");
    if !web_common_dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            web_common_dir.to_string_lossy(),
        ));
    }
    Ok(web_common_dir)
}

/// Gets the `static` dir
fn static_dir() -> std::io::Result<PathBuf> {
    let static_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
    if !static_dir.is_dir() {
        tracing::error!(
            "Couldn't find static directory, searched `{}`",
            static_dir.display()
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            static_dir.to_string_lossy(),
        ));
    }

    Ok(static_dir)
}

/// Generates the default state (all lints, the current version, etc.), and
/// dumps it into [`static_dir()`].
fn dump_default_state_json(sprocket_dir: &Path) -> anyhow::Result<()> {
    let rules = all_rules(sprocket_dir).context("failed to generate rule list")?;
    let default_tags = all_tags(sprocket_dir).context("failed to generate tag list")?;

    let mut all_lints = rules
        .iter()
        .filter(|rule| rule.source == LintSource::WdlLint)
        .collect::<Vec<_>>();
    all_lints.sort_by_key(|rule| rule.id.clone());

    let mut all_analysis_lints = rules
        .iter()
        .filter(|rule| rule.source == LintSource::WdlAnalysis)
        .collect::<Vec<_>>();
    all_analysis_lints.sort_by_key(|rule| rule.id.clone());

    let json = json!({
        "defaultTab": "wdl-lint",
        "defaultTags": default_tags,
        "wdlLint": {
            "allLints": all_lints.into_iter().map(Rule::to_json).collect::<Vec<_>>(),
            "currentVersion": format!("wdl-lint {}", latest_version_of("wdl-lint", sprocket_dir)?),
        },
        "wdlAnalysis": {
            "allLints": all_analysis_lints.into_iter().map(Rule::to_json).collect::<Vec<_>>(),
            "currentVersion": format!("wdl-analysis {}", latest_version_of("wdl-analysis", sprocket_dir)?),
        }
    });

    let output_path = static_dir()?.join("default-state.json");
    std::fs::write(output_path, json.to_string())?;
    Ok(())
}

/// Handles the compilation of the files external to this crate.
fn compile_external(sprocket_dir: &Path) -> std::io::Result<()> {
    fn npm_install(dir: impl AsRef<Path>) -> std::io::Result<()> {
        let dir = dir.as_ref();
        tracing::info!("running `npm install` in {}", dir.display());
        let output = Command::new("npm")
            .arg("install")
            .current_dir(dir)
            .output()?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            tracing::error!("failed to run `npm install`: {err}");
            return Err(std::io::Error::other(err));
        }

        Ok(())
    }

    fn build_js(dir: impl AsRef<Path>) -> std::io::Result<()> {
        let dir = dir.as_ref();
        tracing::info!("compiling JS in `{}`", dir.display());
        let output = Command::new("npm")
            .args(["run", "build"])
            .current_dir(dir)
            .output()?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            tracing::error!("failed to run `npm run build`: {err}",);
            return Err(std::io::Error::other(err));
        }

        Ok(())
    }

    npm_install(web_common_dir(sprocket_dir)?)?;
    npm_install(env!("CARGO_MANIFEST_DIR"))?;

    tracing::info!("generating CSS via `tailwind`");
    let output = Command::new("npm")
        .args(["run", "dist"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        tracing::error!("failed to run `npm run dist`: {err}");
        return Err(std::io::Error::other(err));
    }

    build_js(web_common_dir(sprocket_dir)?)?;
    build_js(env!("CARGO_MANIFEST_DIR"))?;

    Ok(())
}

/// Copies all compiled files recursively from [`web_common_dir()`] and
/// `./static` into `dist_dir`.
fn copy_files_to_dist(dist_dir: &Path, sprocket_dir: &Path) -> std::io::Result<()> {
    fn do_copy(src: &Path, dist_dir: &Path, skip_js: bool) -> std::io::Result<()> {
        for entry in WalkDir::new(src).min_depth(1) {
            let entry = entry?;

            // Skip any CSS, since that's handled by tailwind
            if entry.path().extension().and_then(OsStr::to_str) == Some("css") {
                continue;
            }

            if skip_js && entry.path().extension().and_then(OsStr::to_str) == Some("js") {
                continue;
            }

            let relative_path = entry.path().strip_prefix(src).unwrap();
            let dist_mapped_path = dist_dir.join(relative_path);

            if entry.metadata()?.is_dir() {
                std::fs::create_dir_all(dist_mapped_path)?;
                continue;
            }

            std::fs::copy(entry.path(), dist_mapped_path)?;
        }

        Ok(())
    }

    let static_dir = static_dir()?;

    let web_common_dist_dir = web_common_dir(sprocket_dir)?.join("dist");
    if !web_common_dist_dir.is_dir() {
        tracing::error!(
            "couldn't find web-common/dist, searched `{}`",
            web_common_dist_dir.display()
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            web_common_dist_dir.to_string_lossy(),
        ));
    }

    // skip JS, since esbuild handles it for us
    do_copy(&static_dir, dist_dir, true)?;
    do_copy(&web_common_dist_dir, dist_dir, false)?;

    Ok(())
}
