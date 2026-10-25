use std::collections::HashMap;
use std::process::{Command, ExitCode};
use std::thread::sleep;
use std::time::Duration;

use clap::Parser;
use serde_json::Value;

/// Publish every crate of a cargo workspace in dependency order.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Print the plan and run `cargo publish --dry-run` instead of publishing
    #[arg(long)]
    dry_run: bool,
    /// Crates to publish per batch after the initial burst
    #[arg(long, default_value_t = 3)]
    batch: usize,
    /// Seconds to pause after the initial burst and between batches
    #[arg(long, default_value_t = 240)]
    wait: u64,
    /// Crates to publish before the first pause
    #[arg(long, default_value_t = 30)]
    burst: usize,
    /// Registry to publish to. Custom registries are forwarded without a crates.io check
    #[arg(long)]
    registry: Option<String>,
    /// Ignore whether crate versions are already published
    #[arg(long)]
    ignore_published: bool,
    /// Extra arguments forwarded to `cargo publish`
    #[arg(last = true)]
    cargo_args: Vec<String>,
}

impl Args {
    fn should_pause(&self, published: usize, total: usize) -> bool {
        published < total
            && published >= self.burst
            && (published - self.burst) % self.batch.max(1) == 0
    }
}

const USER_AGENT: &str = concat!("letmerelease/", env!("CARGO_PKG_VERSION"));

struct Crate {
    name: String,
    version: String,
    workspace_deps: Vec<String>,
}

impl Crate {
    /// Reads the publishable workspace members through `cargo metadata`.
    fn workspace() -> Result<Vec<Crate>, String> {
        let output = Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .map_err(|error| format!("failed to run cargo metadata: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        let metadata: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid cargo metadata: {error}"))?;

        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &Value) -> Vec<Crate> {
        let packages = metadata["packages"].as_array().into_iter().flatten();
        let publishable: Vec<&str> = packages
            .clone()
            .filter(|package| {
                package["publish"]
                    .as_array()
                    .is_none_or(|registries| !registries.is_empty())
            })
            .filter_map(|package| package["name"].as_str())
            .collect();

        let mut crates: Vec<Crate> = packages
            .filter(|package| {
                package["publish"]
                    .as_array()
                    .is_none_or(|registries| !registries.is_empty())
            })
            .map(|package| {
                let mut workspace_deps: Vec<String> = package["dependencies"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|dependency| {
                        dependency["path"].is_string()
                            && (dependency["kind"] != "dev"
                                || dependency["req"].as_str().is_some_and(|req| req != "*"))
                    })
                    .filter_map(|dependency| dependency["name"].as_str())
                    .filter(|name| publishable.contains(name))
                    .map(str::to_owned)
                    .collect();
                workspace_deps.sort_unstable();
                workspace_deps.dedup();
                Crate {
                    name: package["name"].as_str().unwrap_or_default().to_owned(),
                    version: package["version"].as_str().unwrap_or_default().to_owned(),
                    workspace_deps,
                }
            })
            .collect();
        crates.sort_by(|left, right| {
            left.workspace_deps
                .len()
                .cmp(&right.workspace_deps.len())
                .then_with(|| left.name.cmp(&right.name))
        });
        crates
    }
}

/// Orders crates so every crate comes after its workspace dependencies.
fn publish_order(mut crates: Vec<Crate>) -> Result<Vec<String>, String> {
    crates.sort_by(|left, right| {
        left.workspace_deps
            .len()
            .cmp(&right.workspace_deps.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut pending: HashMap<&str, usize> = crates
        .iter()
        .map(|krate| (krate.name.as_str(), krate.workspace_deps.len()))
        .collect();
    let mut order = Vec::with_capacity(crates.len());

    while order.len() < crates.len() {
        let Some(next) = crates
            .iter()
            .filter(|krate| pending.get(krate.name.as_str()) == Some(&0))
            .min_by(|left, right| {
                left.workspace_deps
                    .len()
                    .cmp(&right.workspace_deps.len())
                    .then_with(|| left.name.cmp(&right.name))
            })
            .map(|krate| krate.name.clone())
        else {
            return Err("dependency cycle between workspace crates".to_owned());
        };
        pending.remove(next.as_str());
        for krate in crates
            .iter()
            .filter(|krate| krate.workspace_deps.contains(&next))
        {
            if let Some(count) = pending.get_mut(krate.name.as_str()) {
                *count -= 1;
            }
        }
        order.push(next);
    }
    Ok(order)
}

/// Builds the crates.io sparse-index path for a crate name.
/// See <https://doc.rust-lang.org/cargo/reference/registry-index.html#index-files>.
fn index_path(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.len() {
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", &lower[..1]),
        _ => format!("{}/{}/{lower}", &lower[..2], &lower[2..4]),
    }
}

/// Queries the crates.io sparse index and returns `true` if `version`
/// (yanked or not) already exists for `name`.
fn is_published(name: &str, version: &str) -> Result<bool, String> {
    let url = format!("https://index.crates.io/{}", index_path(name));
    let response = match ureq::get(&url).set("User-Agent", USER_AGENT).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return Ok(false),
        Err(error) => return Err(format!("index request for {name} failed: {error}")),
    };
    let body = response
        .into_string()
        .map_err(|error| format!("read index response for {name}: {error}"))?;
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid index entry for {name}: {error}"))?;
        if entry["vers"].as_str() == Some(version) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cargo_publish(args: &[&str], extra: &[String]) -> Result<(), String> {
    let status = Command::new("cargo")
        .arg("publish")
        .args(args)
        .args(extra)
        .status()
        .map_err(|error| format!("failed to run cargo publish: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo publish failed with {status}"))
    }
}

fn run(args: Args) -> Result<(), String> {
    let crates = Crate::workspace()?;
    let versions: HashMap<String, String> = crates
        .iter()
        .map(|krate| (krate.name.clone(), krate.version.clone()))
        .collect();
    let order = publish_order(crates)?;
    if order.is_empty() {
        return Err("no publishable crates found".to_owned());
    }

    let mut forwarded_args = Vec::with_capacity(args.cargo_args.len() + 2);
    if let Some(registry) = &args.registry {
        forwarded_args.push("--registry".to_owned());
        forwarded_args.push(registry.clone());
    }
    forwarded_args.extend(args.cargo_args.iter().cloned());

    let check_crates_io = args
        .registry
        .as_deref()
        .is_none_or(|registry| registry == "crates-io");
    let mut pending: Vec<String> = Vec::with_capacity(order.len());
    let mut skipped: Vec<String> = Vec::new();
    for name in &order {
        let version = versions
            .get(name)
            .ok_or_else(|| format!("missing version for {name}"))?;
        if check_crates_io && !args.ignore_published && is_published(name, version)? {
            skipped.push(format!("{name}@{version}"));
        } else {
            pending.push(name.clone());
        }
    }

    if !skipped.is_empty() {
        println!("Skipping {} already-published crates:", skipped.len());
        for name in &skipped {
            println!("  - {name}");
        }
    }

    if pending.is_empty() {
        println!("Nothing left to publish.");
        if args.dry_run {
            let packages: Vec<&str> = order
                .iter()
                .flat_map(|name| ["-p", name.as_str()])
                .collect();
            return cargo_publish(
                &[packages.as_slice(), &["--dry-run"]].concat(),
                &forwarded_args,
            );
        }
        return Ok(());
    }

    let paced = pending.len() > args.burst;
    println!("Publishing {} crates in this order:", pending.len());
    for (index, name) in pending.iter().enumerate() {
        println!("  {}. {name}", index + 1);
    }
    if paced {
        println!(
            "Publishing an initial burst of {} crates, then pausing {}s before each batch of {}",
            args.burst, args.wait, args.batch
        );
    }

    if args.dry_run {
        let packages: Vec<&str> = pending
            .iter()
            .flat_map(|name| ["-p", name.as_str()])
            .collect();
        return cargo_publish(
            &[packages.as_slice(), &["--dry-run"]].concat(),
            &forwarded_args,
        );
    }

    for (index, name) in pending.iter().enumerate() {
        println!("\nPublishing {name} ({}/{})", index + 1, pending.len());
        cargo_publish(&["-p", name], &forwarded_args)?;
        if args.should_pause(index + 1, pending.len()) {
            println!("Waiting {}s before the next batch", args.wait);
            sleep(Duration::from_secs(args.wait));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::{Args, Crate, index_path, publish_order};

    #[test]
    fn default_schedule_pauses_after_burst_and_batches() {
        let args = Args::parse_from(["letmerelease"]);
        assert_eq!(args.wait, 240);
        let pauses: Vec<usize> = (1..=40)
            .filter(|published| args.should_pause(*published, 40))
            .collect();
        assert_eq!(pauses, [30, 33, 36, 39]);
    }

    #[test]
    fn schedule_never_pauses_after_final_crate() {
        let args = Args::parse_from(["letmerelease"]);
        for total in [1, 20, 30, 31, 33, 36] {
            assert!(!args.should_pause(total, total));
        }
        assert!((1..30).all(|published| !args.should_pause(published, 30)));
    }

    #[test]
    fn schedule_supports_custom_burst_and_batch() {
        let args = Args::parse_from(["letmerelease", "--burst", "5", "--batch", "2"]);
        let pauses: Vec<usize> = (1..=10)
            .filter(|published| args.should_pause(*published, 10))
            .collect();
        assert_eq!(pauses, [5, 7, 9]);
    }

    fn krate(name: &str, deps: &[&str]) -> Crate {
        Crate {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            workspace_deps: deps.iter().map(|dep| dep.to_string()).collect(),
        }
    }

    #[test]
    fn dependencies_come_first() {
        let crates = vec![
            krate("app", &["core", "utils"]),
            krate("utils", &["core"]),
            krate("core", &[]),
        ];
        assert_eq!(publish_order(crates).unwrap(), ["core", "utils", "app"]);
    }

    fn dev_dependency_metadata(requirement: &str) -> serde_json::Value {
        serde_json::json!({
            "packages": [
                {
                    "name": "freya-terminal",
                    "version": "0.5.0-beta.12",
                    "dependencies": [{
                        "name": "freya-testing",
                        "kind": "dev",
                        "path": "/workspace/testing",
                        "req": requirement
                    }]
                },
                {
                    "name": "freya-testing",
                    "version": "0.5.0-beta.12",
                    "dependencies": []
                }
            ]
        })
    }

    #[test]
    fn versioned_dev_dependencies_come_first() {
        let metadata = dev_dependency_metadata("=0.5.0-beta.12");
        assert_eq!(
            publish_order(Crate::from_metadata(&metadata)).unwrap(),
            ["freya-testing", "freya-terminal"]
        );
    }

    #[test]
    fn path_only_dev_dependencies_do_not_affect_order() {
        let metadata = dev_dependency_metadata("*");
        let crates = Crate::from_metadata(&metadata);
        assert!(crates.iter().all(|krate| krate.workspace_deps.is_empty()));
    }

    #[test]
    fn versioned_dev_dependency_cycles_are_rejected() {
        let mut metadata = dev_dependency_metadata("=0.5.0-beta.12");
        metadata["packages"][1]["dependencies"] = serde_json::json!([{
            "name": "freya-terminal",
            "kind": null,
            "path": "/workspace/terminal",
            "req": "=0.5.0-beta.12"
        }]);
        assert!(publish_order(Crate::from_metadata(&metadata)).is_err());
    }

    #[test]
    fn cycles_are_rejected() {
        let crates = vec![krate("a", &["b"]), krate("b", &["a"])];
        assert!(publish_order(crates).is_err());
    }

    #[test]
    fn index_path_matches_registry_spec() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("serde"), "se/rd/serde");
        assert_eq!(index_path("Tokio"), "to/ki/tokio");
    }
}
