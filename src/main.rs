use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufReader, BufWriter, Read},
    process::{ChildStderr, ChildStdout, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::Style,
    widgets::{Block, Borders, Paragraph, Widget},
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

enum ChildOutput {
    Out(ChildStdout),
    Err(ChildStderr),
}

impl std::io::Read for ChildOutput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ChildOutput::Out(child_stdout) => child_stdout.read(buf),
            ChildOutput::Err(child_stderr) => child_stderr.read(buf),
        }
    }
}
#[derive(Default, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
struct Command {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

impl Command {
    fn run(self, rows: u16, cols: u16) -> Result<String, String> {
        // Use the native pty implementation for the system
        let pty_system = portable_pty::native_pty_system();
        let terminal = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let cmd = CommandBuilder::new(self.command.clone());
        let out = terminal.slave.spawn_command(cmd);
        drop(terminal.slave);

        let mut child = out.map_err(|e| e.to_string())?;

        child.wait().map_err(|e| e.to_string())?;
        drop(terminal.master.take_writer());
        eprintln!("here");
        let mut buf = String::new();
        terminal
            .master
            .try_clone_reader()
            .map_err(|e| e.to_string())?
            .read_to_string(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(buf)
    }
}

#[derive(Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(untagged)]
enum Element {
    String(String),
    Grid(DasboardLayout),
    Command(Command),
}

impl Default for Element {
    fn default() -> Self {
        Self::String(Default::default())
    }
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
struct DashboardColumn {
    percent: Option<u16>,
    rows: Vec<DashboardWidget>,
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
struct DashboardWidget {
    element: Element,
    percent: Option<u16>,
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone, PartialEq, Eq)]
struct DasboardLayout {
    columns: Vec<DashboardColumn>,
}

impl Widget for Element {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self {
            Element::String(name) => Paragraph::new(name.clone())
                .block(Block::new().borders(Borders::ALL))
                .render(area, buf),
            Element::Grid(dasboard_layout) => dasboard_layout.clone().render(area, buf),
            Element::Command(command) => {
                let command_str = format!("{} {}", command.command, command.args.join(" "));
                let out = command
                    .run(area.width, area.height)
                    .unwrap_or_else(|e| e.to_string());
                Paragraph::new(out)
                    .block(Block::new().borders(Borders::ALL).title(command_str))
                    .render(area, buf)
            }
        };
    }
}

impl Widget for DasboardLayout {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let cols_layout =
            Layout::horizontal(self.columns.iter().map(|DashboardColumn { percent, .. }| {
                Constraint::Percentage(percent.unwrap_or(100))
            }))
            .spacing(0)
            .margin(0)
            .split(area);
        assert_eq!(cols_layout.iter().len(), self.columns.len());
        self.columns.iter().enumerate().for_each(
            |(col_i, DashboardColumn { rows: columns, .. })| {
                columns
                    .iter()
                    .zip(
                        Layout::vertical(
                            columns
                                .iter()
                                .map(|row| Constraint::Percentage(row.percent.unwrap_or(100))),
                        )
                        .spacing(0)
                        .margin(0)
                        .split(cols_layout[col_i])
                        .iter(),
                    )
                    .for_each(|(widget, layout)| {
                        widget.element.clone().render(*layout, buf);
                    })
            },
        )
    }
}

#[derive(Default)]
struct App {
    exit: bool,
    layout: DasboardLayout,
}

impl App {
    fn with_layout(self, layout: DasboardLayout) -> Self {
        Self { layout, ..self }
    }
}

impl App {
    fn render(&self, frame: &mut Frame) {
        frame.render_widget(self.layout.clone(), frame.area());
    }

    fn quit(&mut self) {
        self.exit = true
    }

    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        configuration_update: Receiver<DasboardLayout>,
    ) -> Result<(), std::io::Error> {
        terminal.clear()?;
        while !self.exit {
            configuration_update.try_iter().for_each(|layout| {
                terminal.clear().unwrap_or_default();
                self.layout = layout
            });
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(100))? {
                let event = event::read()?;
                if let Event::Key(key) = event
                    && let KeyEventKind::Press = key.kind
                    && let KeyCode::Char('q') = key.code
                {
                    self.quit()
                }
            }
        }
        Ok(())
    }
}

// fn collect_commands(layout: &DasboardLayout) -> impl Iterator<Item = Vec<Command>> {
//     layout.columns.iter().map(|col| {
//
//         col.rows.into_iter()
//         // col.rows.iter().filter_map(|row| match &row.element {
//         //     Element::Command(command) => Some(vec![command.clone()]),
//         //     Element::Grid(layout) => Some(collect_commands(layout).into_iter()),
//         //     _ => None,
//         // })
//     })
// }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pty_system = NativePtySystem::default();

    // Create a new pty
    let mut pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Spawn a shell into the pty
    let command = CommandBuilder::new("nu");
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);

    // Read and parse output from the pty with reader
    let mut reader = pair.master.try_clone_reader()?;

    // Send data to the pty by writing to the master
    writeln!(pair.master.take_writer()?, "ls\r\n\x04")?;
    drop(pair.master);

    let mut out = String::new();
    reader.read_to_string(&mut out)?;
    println!("{}", out);

    println!("{}", child.wait()?);
    Ok(())
}
fn _main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = "dashboard.schema.json";
    serde_json::to_writer_pretty(
        BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(schema)?,
        ),
        &schema_for!(DasboardLayout),
    )?;
    let configuration = "dashboard.json";
    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    // Use the native pty implementation for the system
    let pty_system = portable_pty::native_pty_system();

    let mut emulator = pty_system.openpty(PtySize {
        rows: size.width,
        cols: size.height,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Spawn a shell into the pty
    let mut cmd = CommandBuilder::new("nu");
    cmd.args(["-c", "ls"]);

    let mut child = emulator.slave.spawn_command(cmd)?;

    // Read and parse output from the pty with reader
    let mut reader = emulator.master.try_clone_reader()?;
    //
    // // Send data to the pty by writing to the master
    // writeln!(emulator.master.take_writer()?, "ls -l\r\n")?;
    //
    let mut out = String::new();
    reader.read_to_string(&mut out)?;
    println!("{out}");

    ratatui::restore();
    // if std::fs::exists(configuration)? {
    //     let (send, recv) = mpsc::channel::<DasboardLayout>();
    //     let mut app = App::default().with_layout(serde_json::from_reader(BufReader::new(
    //         OpenOptions::new().read(true).open(configuration)?,
    //     ))?);
    //     // TODO: join thread before app quits
    //     let _ = std::thread::spawn(move || {
    //         let mut contents = std::fs::read_to_string(configuration).unwrap_or_default();
    //         loop {
    //             let new_contents = std::fs::read_to_string(configuration).unwrap_or_default();
    //             if new_contents != contents {
    //                 contents = new_contents;
    //                 if let Ok(configuration) = serde_json::from_str(&contents)
    //                     && let Err(_) = send.send(configuration)
    //                 {
    //                     break;
    //                 }
    //             }
    //         }
    //     });
    //
    //     let command = Command {
    //         command: "nu".to_string(),
    //         ..Default::default()
    //     };
    //
    //     command.run(terminal.size()?.width, terminal.size()?.height)?;
    //
    //     // app.run(&mut terminal, recv)?;
    //     ratatui::restore();
    //
    //     Ok(())
    // } else {
    //     Err("no configuration")?
    // }
    Ok(())
}
