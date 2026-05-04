use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufReader, BufWriter, Read},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use ansi_to_tui::IntoText as _;
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    widgets::{Block, Borders, Paragraph, Widget},
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

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
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};

        let pty_system = native_pty_system();

        let emulator = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;

        let mut cmd = CommandBuilder::new(self.command);
        cmd.cwd(std::env::current_dir().map_err(|e| e.to_string())?);
        cmd.args(self.args);
        let mut child = emulator
            .slave
            .spawn_command(cmd)
            .map_err(|e| e.to_string())?;

        child.wait().map_err(|e| e.to_string())?;

        // dropping the slave will send a EOF to the terminal, allowing the read to happen without
        // blocking?
        drop(emulator.slave);

        let mut reader = emulator
            .master
            .try_clone_reader()
            .map_err(|e| e.to_string())?;
        let mut buf = String::new();
        reader.read_to_string(&mut buf).map_err(|e| e.to_string())?;

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
                // TODO: don't run on every render
                let command_str = format!("{} {}", command.command, command.args.join(" "));
                let out = command
                    .run(area.height, area.width)
                    .unwrap_or_else(|e| e.to_string());
                let text = out.as_str().into_text().unwrap_or_default();
                Paragraph::new(text)
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
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    if std::fs::exists(configuration)? {
        let mut terminal = ratatui::init();
        let (send, recv) = mpsc::channel::<DasboardLayout>();
        let mut app = App::default().with_layout(serde_json::from_reader(BufReader::new(
            OpenOptions::new().read(true).open(configuration)?,
        ))?);
        // TODO: join thread before app quits
        let _ = std::thread::spawn(move || {
            let mut contents = std::fs::read_to_string(configuration).unwrap_or_default();
            loop {
                let new_contents = std::fs::read_to_string(configuration).unwrap_or_default();
                if new_contents != contents {
                    contents = new_contents;
                    if let Ok(configuration) = serde_json::from_str(&contents)
                        && let Err(_) = send.send(configuration)
                    {
                        break;
                    }
                }
            }
        });

        app.run(&mut terminal, recv)?;
        ratatui::restore();
        Ok(())
    } else {
        Err("no configuration")?
    }
}
