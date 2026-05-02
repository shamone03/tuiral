use std::{
    fs::OpenOptions,
    io::{BufReader, BufWriter},
};

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

#[derive(Deserialize, Serialize, JsonSchema, Clone)]
#[serde(untagged)]
enum Element {
    String(String),
    Grid(DasboardLayout),
}

impl Default for Element {
    fn default() -> Self {
        Self::String(Default::default())
    }
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone)]
struct DashboardColumn {
    percent: Option<u16>,
    rows: Vec<DashboardWidget>,
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone)]
struct DashboardWidget {
    element: Element,
    percent: Option<u16>,
}

#[derive(Default, Deserialize, Serialize, JsonSchema, Clone)]
struct DasboardLayout {
    columns: Vec<DashboardColumn>,
}
impl Widget for DasboardLayout {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let cols_layout =
            Layout::horizontal(self.columns.iter().map(|DashboardColumn { percent, .. }| {
                Constraint::Percentage(percent.unwrap_or(100))
            }))
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
                        .split(cols_layout[col_i])
                        .iter(),
                    )
                    .for_each(|(widget, layout)| {
                        match &widget.element {
                            Element::String(name) => Paragraph::new(name.clone())
                                .block(
                                    Block::new()
                                        .borders(Borders::ALL)
                                        .border_style(Style::new().green()),
                                )
                                .render(*layout, buf),
                            Element::Grid(dasboard_layout) => {
                                dasboard_layout.clone().render(*layout, buf)
                            }
                        };
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
        self.layout.clone().render(frame.area(), frame.buffer_mut());
    }

    fn quit(&mut self) {
        self.exit = true
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), std::io::Error> {
        terminal.clear()?;
        while !self.exit {
            terminal.draw(|frame| self.render(frame))?;
            let event = event::read()?;
            if let Event::Key(key) = event
                && let KeyEventKind::Press = key.kind
                && let KeyCode::Char('q') = key.code
            {
                self.quit()
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
        App::default()
            .with_layout(serde_json::from_reader(BufReader::new(
                OpenOptions::new().read(true).open(configuration)?,
            ))?)
            .run(&mut terminal)?;
        ratatui::restore();

        Ok(())
    } else {
        Err("no configuration")?
    }
}
