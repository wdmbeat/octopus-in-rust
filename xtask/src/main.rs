use anyhow::Result;

mod apps;
mod args;
mod cargo;
mod deploy;
mod plugins;
mod project;
#[allow(dead_code)]
mod web;

fn main() -> Result<()> {
    match args::Task::parse()? {
        args::Task::App { app, command, rest } => match app {
            args::App::Fafcn => apps::fafcn::run(&command, &rest),
            args::App::FafSim => apps::fafsim::run(&command, &rest),
            args::App::Qqbot => apps::qqbot::run(&command, &rest),
            args::App::FafMl => apps::fafml::run(&command, &rest),
        },
        args::Task::Global(args::GlobalCommand::Test) => cargo::test_workspace(),
    }
}
