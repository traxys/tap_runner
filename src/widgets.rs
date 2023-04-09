use tui::{
    style::Color,
    widgets::{Block, Widget},
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
