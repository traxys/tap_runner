use tui::{
    backend::Backend,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Widget},
    Frame,
};

pub struct ColoredList<'a> {
    colors: Vec<Color>,
    block: Option<Block<'a>>,
}

impl<'a> ColoredList<'a> {
    pub fn new(colors: Vec<Color>) -> Self {
        Self {
            colors,
            block: None,
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Widget for ColoredList<'_> {
    fn render(mut self, area: tui::layout::Rect, buf: &mut tui::buffer::Buffer) {
        let list_area = match self.block.take() {
            Some(b) => {
                let inner_area = b.inner(area);
                b.render(area, buf);
                inner_area
            }
            None => area,
        };

        let available_space = list_area.area() as usize;

        let (print_count, lack_space) = if available_space < self.colors.len() {
            (available_space - 3, true)
        } else {
            (self.colors.len(), false)
        };

        for (idx, &c) in self.colors[..print_count].iter().enumerate() {
            let x = idx as u16 % list_area.width;
            let y = idx as u16 / list_area.width;

            buf.get_mut(list_area.left() + x, list_area.top() + y)
                .set_symbol(" ")
                .set_bg(c);
        }

        if lack_space {
            for i in 0..3 {
                buf.get_mut(list_area.right() - i, list_area.bottom())
                    .set_symbol(".");
            }
        }
    }
}

pub struct StatefulList<T> {
    state: ListState,
    items: Vec<T>,
}

impl<T> StatefulList<T> {
    pub fn empty() -> Self {
        Self::with_items(Vec::new())
    }

    pub fn render<B, F>(&mut self, frame: &mut Frame<B>, area: Rect, make_item: F)
    where
        B: Backend,
        F: FnMut(&T) -> ListItem,
    {
        frame.render_stateful_widget(
            List::new(Vec::from_iter(self.items.iter().map(make_item)))
                .highlight_style(Style::default().bg(Color::Rgb(0x33, 0x46, 0x7c))),
            area,
            &mut self.state,
        )
    }

    pub fn with_items(items: Vec<T>) -> StatefulList<T> {
        StatefulList {
            state: ListState::default(),
            items,
        }
    }

    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn unselect(&mut self) {
        self.state.select(None);
    }
}
