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
    /// Crates to publish before pausing, only used in large workspaces
    #[arg(long, default_value_t = 5)]
    batch: usize,
    /// Seconds to pause between batches
    #[arg(long, default_value_t = 180)]
    wait: u64,
    /// Workspaces with more crates than this get paused between batches
    #[arg(long, default_value_t = 20)]
    large: usize,
    /// Extra arguments forwarded to `cargo publish`
    #[arg(last = true)]
    cargo_args: Vec<String>,
}

struct Crate {
    name: String,
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

        let crates = packages
            .filter(|package| {
                package["publish"]
                    .as_array()
                    .is_none_or(|registries| !registries.is_empty())
            })
            .map(|package| Crate {
                name: package["name"].as_str().unwrap_or_default().to_owned(),
                workspace_deps: package["dependencies"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|dependency| {
                        dependency["kind"] != "dev" && dependency["path"].is_string()
                    })
                    .filter_map(|dependency| dependency["name"].as_str())
                    .filter(|name| publishable.contains(name))
                    .map(str::to_owned)
                    .collect(),
            })
            .collect();
        Ok(crates)
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
    let order = publish_order(Crate::workspace()?)?;
    if order.is_empty() {
        return Err("no publishable crates found".to_owned());
    }
    let paced = order.len() > args.large;

    println!("Publishing {} crates in this order:", order.len());
    for (index, name) in order.iter().enumerate() {
        println!("  {}. {name}", index + 1);
    }
    if paced {
        println!(
            "Large workspace, pausing {}s after every {} crates",
            args.wait, args.batch
        );
    }

    if args.dry_run {
        let packages: Vec<&str> = order.iter().flat_map(|name| ["-p", name]).collect();
        return cargo_publish(
            &[packages.as_slice(), &["--dry-run"]].concat(),
            &args.cargo_args,
        );
    }

    for (index, name) in order.iter().enumerate() {
        println!("\nPublishing {name} ({}/{})", index + 1, order.len());
        cargo_publish(&["-p", name], &args.cargo_args)?;
        let batch_done = (index + 1) % args.batch.max(1) == 0;
        if paced && batch_done && index + 1 < order.len() {
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
    use crate::{Crate, publish_order};

    fn krate(name: &str, deps: &[&str]) -> Crate {
        Crate {
            name: name.to_owned(),
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

    #[test]
    fn cycles_are_rejected() {
        let crates = vec![krate("a", &["b"]), krate("b", &["a"])];
        assert!(publish_order(crates).is_err());
    }
}
