//! CosmicSlicer desktop application: a slicer for FFF 3D printers.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
#![allow(non_snake_case)]

use std::path::PathBuf;
use std::process::ExitCode;

use CosmicSlicer_ui_egui::{Automation, LaunchOptions, Step};

const USAGE: &str = "usage: CosmicSlicer [model.stl ...] [options]

  model.stl / model.obj   import these models
  --tab <page>            open on Home, Prepare, Preview, Device, Multi-device or Project
  --add <primitive>       add a Cube, Cylinder, Sphere, Cone, Disc or Torus (repeatable)
  --arrange               arrange the plate
  --select-all            select every object
  --theme <dark|light>    use this theme
  --dialog <name>         open preferences, printer, filament, process, compare, shortcuts or about
  --tool <name>           activate a canvas tool (move, rotate, scale, ...)
  --page <name>           show this process settings page (Quality, Strength, ...)
  --advanced              show all settings
  --set <key=value>       change a process setting, e.g. --set sparse_infill_density=40%
  --slice                 slice the plate and show the Preview
  --export <file.gcode>   slice the plate and write its G-code
  --layer <n>             show the Preview up to layer n
  --screenshot <file>     save a PNG of the window once the steps have run, then quit
                          (a screenshot run saves nothing, unless COSMICSLICER_DATA_DIR
                          points it at a folder of its own)

Recovery testing (with COSMICSLICER_DATA_DIR set to a scratch folder):
  --autosave              autosave now
  --simulate-crash        stop at once, as a crash would, leaving the autosave behind
  --restore               answer Restore to the restore prompt
  --simulate-panic        fail inside a frame: the app recovers and writes a crash report";

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut steps = Vec::new();
    let mut files = Vec::new();
    let mut screenshot = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let step = match arg.as_str() {
            "--tab" => value("--tab").map(Step::Tab),
            "--add" => value("--add").map(Step::Primitive),
            "--theme" => value("--theme").map(Step::Theme),
            "--dialog" => value("--dialog").map(Step::Dialog),
            "--tool" => value("--tool").map(Step::Tool),
            "--page" => value("--page").map(Step::Page),
            "--set" => value("--set").map(Step::Set),
            "--autosave" => Ok(Step::Autosave),
            "--simulate-crash" => Ok(Step::SimulateCrash),
            "--restore" => Ok(Step::Restore),
            "--simulate-panic" => Ok(Step::SimulatePanic),
            "--layer" => value("--layer")
                .and_then(|v| v.parse().map(Step::Layer).map_err(|_| format!("--layer needs a number, not {v}"))),
            "--export" => value("--export").map(|p| Step::Export(PathBuf::from(p))),
            "--arrange" => Ok(Step::Arrange),
            "--select-all" => Ok(Step::SelectAll),
            "--advanced" => Ok(Step::Advanced),
            "--slice" => Ok(Step::Slice),
            "--screenshot" => match value("--screenshot") {
                Ok(path) => {
                    screenshot = Some(PathBuf::from(path));
                    continue;
                }
                Err(e) => Err(e),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            path if !path.starts_with('-') => {
                files.push(PathBuf::from(path));
                continue;
            }
            other => Err(format!("unknown argument: {other}")),
        };
        match step {
            Ok(step) => steps.push(step),
            Err(e) => {
                eprintln!("{e}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    // A screenshot run is a test: it must not change the user's saved settings, unless it was
    // given a data folder of its own.
    let ephemeral = screenshot.is_some() && std::env::var_os("COSMICSLICER_DATA_DIR").is_none_or(|d| d.is_empty());
    // Files named on the command line load before the steps run.
    let steps: Vec<Step> = files.drain(..).map(Step::Open).chain(steps).collect();
    let options = LaunchOptions { automation: Automation::new(steps, screenshot), ephemeral, files };
    match CosmicSlicer_ui_egui::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("CosmicSlicer could not start: {e}");
            ExitCode::FAILURE
        }
    }
}
