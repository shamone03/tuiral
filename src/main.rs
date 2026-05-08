use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufReader, BufWriter, Read, Write},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use ansi_to_tui::IntoText;
use crossterm::event::KeyModifiers;
use portable_pty::Child;
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    crossterm::event::{self, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    widgets::{Block, Borders, Paragraph, Widget},
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

enum Event {
    Quit,
    LayoutUpdate(DasboardLayout),
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
    fn to_string(&self) -> String {
        format!("{} {}", self.command, self.args.join(" "))
    }
}

struct CommandRunner {
    reader: Receiver<Vec<u8>>,
    writer: Sender<Vec<u8>>,
    writer_handle: JoinHandle<()>,
    reader_handle: JoinHandle<()>,
    child: Box<dyn Child + Send + Sync>,
}

impl CommandRunner {
    fn new(command: Command, rows: u16, cols: u16) -> Result<Self, String> {
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

        let mut cmd = CommandBuilder::new(command.clone().command);
        cmd.cwd(std::env::current_dir().map_err(|e| e.to_string())?);
        cmd.args(command.clone().args);
        let child = emulator
            .slave
            .spawn_command(cmd)
            .map_err(|e| e.to_string())?;
        drop(emulator.slave);

        let mut emulator_reader = emulator
            .master
            .try_clone_reader()
            .map_err(|e| e.to_string())?;

        let mut emulator_writer = emulator.master.take_writer().map_err(|e| e.to_string())?;
        let (command_writer, input) = mpsc::channel::<Vec<u8>>();
        let writer_handle = std::thread::Builder::new()
            .name(format!("reading {} input", command.clone().to_string()))
            .spawn(move || {
                input.iter().for_each(|input| {
                    emulator_writer.write_all(&input).unwrap();
                });
            })
            .map_err(|e| e.to_string())?;

        let (output, command_reader) = mpsc::channel::<Vec<u8>>();
        let reader_handle = std::thread::Builder::new()
            .name(format!("reading {} output", command.to_string()))
            .spawn(move || {
                let mut buffer = [0u8; 1024];
                loop {
                    match emulator_reader.read(&mut buffer) {
                        Ok(0) => panic!(), // EOF
                        Ok(n) => {
                            output.send(buffer[..n].to_vec()).unwrap();
                        }
                        Err(e) => {
                            panic!()
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(Self {
            reader: command_reader,
            writer: command_writer,
            reader_handle,
            writer_handle,
            child,
        })
    }
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

struct App {
    exit: bool,
    layout: DasboardLayout,
    events: Receiver<Event>,
    commands: HashMap<String, (Receiver<Vec<u8>>, Box<dyn Child + Send + Sync>)>,
    command_buffer: Vec<u8>,
}

impl App {
    fn new(
        events: Receiver<Event>,
        commands: HashMap<String, (Receiver<Vec<u8>>, Box<dyn Child + Send + Sync>)>,
    ) -> Self {
        Self {
            exit: false,
            layout: DasboardLayout::default(),
            events,
            commands,
            command_buffer: Default::default(),
        }
    }
    fn with_layout(self, layout: DasboardLayout) -> Self {
        Self { layout, ..self }
    }
}

impl App {
    fn render(&self, frame: &mut Frame) {
        frame.render_widget(self.layout.clone(), frame.area());
    }

    fn process_event(&mut self, terminal: &mut DefaultTerminal) {
        self.events.try_iter().for_each(|e| match e {
            Event::LayoutUpdate(layout) => {
                terminal.clear().unwrap_or_default();
                self.layout = layout
            }
            Event::Quit => self.exit = true,
        });
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        terminal.clear()?;
        while !self.exit {
            self.process_event(terminal);
            self.commands.iter_mut().for_each(|(id, (recv, child))| {
                match child.try_wait() {
                    Ok(status) => {
                        if let Some(status) = status {
                            print!("exited {}", status.exit_code());
                            self.exit = true
                        }
                    }
                    Err(err) => panic!(),
                }
                match recv.try_recv() {
                    Ok(mut msg) => {
                        self.command_buffer.append(&mut msg);
                    }
                    Err(e) => match e {
                        mpsc::TryRecvError::Empty => {}
                        mpsc::TryRecvError::Disconnected => { self.command_buffer = String::from("process exited").into_bytes(); },
                    },
                }
            });
            terminal.try_draw(|frame| {
                self.command_buffer
                    .to_text()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
                    .render(frame.area(), frame.buffer_mut());
                Ok::<_, std::io::Error>(())
            })?;
            // terminal.draw(|frame| self.render(frame))?;
        }
        Ok(())
    }
}

fn collect_commands(layout: DasboardLayout) -> Vec<Command> {
    layout
        .columns
        .into_iter()
        .flat_map(|col| {
            col.rows
                .into_iter()
                .map(|row| row.element)
                .filter_map(|elem| match elem {
                    Element::Command(command) => Some(vec![command]),
                    Element::Grid(layout) => Some(collect_commands(layout)),
                    _ => None,
                })
        })
        .flatten()
        .collect()
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
        let (send, recv) = mpsc::channel::<Event>();

        let layout: DasboardLayout = serde_json::from_reader(BufReader::new(
            OpenOptions::new().read(true).open(configuration)?,
        ))?;
        let size = terminal.size()?;
        let runners = collect_commands(layout.clone()).into_iter().try_fold(
            HashMap::new(),
            |mut accum, command| {
                accum.insert(
                    command.to_string(),
                    CommandRunner::new(command, size.height, size.width)?,
                );
                Ok::<_, String>(accum)
            },
        )?;
        let mut writers = runners
            .iter()
            .map(|(id, command)| (id.clone(), command.writer.clone()))
            .collect::<HashMap<_, _>>();

        // TODO: join thread before app quits
        let _ = std::thread::Builder::new()
            .name("input".to_string())
            .spawn(move || {
                let mut contents = std::fs::read_to_string(configuration).unwrap_or_default();
                loop {
                    let new_contents = std::fs::read_to_string(configuration).unwrap_or_default();
                    if new_contents != contents {
                        contents = new_contents;
                        if let Ok(configuration) = serde_json::from_str::<DasboardLayout>(&contents)
                            && let Err(_) = send.send(Event::LayoutUpdate(configuration))
                        {
                            break;
                        }
                    }
                    if event::poll(Duration::from_millis(100)).is_ok_and(|avail| avail)
                        && let Ok(event) = event::read()
                        && let crossterm::event::Event::Key(key) = event
                        && let KeyEventKind::Press = key.kind
                    {
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && let KeyCode::Char('q') = key.code
                        {
                            let _ = send.send(Event::Quit);
                        }
                        // else if let KeyCode::Char(letter) = key.code {
                        //     writers.iter_mut().for_each(|(_, writer)| {
                        //         let bytes = letter.to_string().bytes().collect::<Vec<_>>();
                        //         writer.send(bytes).unwrap();
                        //     });
                        // } else if let KeyCode::Enter = key.code {
                        //     writers.iter_mut().for_each(|(_, writer)| {
                        //         writer.send("\n".to_string().into_bytes()).unwrap();
                        //     });
                        // }
                    }

                    let mut buffer = [0u8; 1024];
                    match std::io::stdin().read(&mut buffer) {
                        Ok(0) => panic!(), // EOF
                        Ok(n) => {
                            writers.iter().for_each(|(_, writer)| {
                                writer.send(buffer[..n].to_vec()).unwrap();
                            });
                        }
                        Err(e) => {
                            panic!()
                        }
                    }
                }
            });

        let receivers = runners
            .into_iter()
            .map(|(id, runner)| (id.to_string(), (runner.reader, runner.child)))
            .collect();
        let mut app = App::new(recv, receivers).with_layout(layout);

        app.run(&mut terminal)?;
        ratatui::restore();
        Ok(())
    } else {
        Err("no configuration")?
    }
}
