use crate::core::Rectangle;

/// A fixed reference line for the measurement of coordinates.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum Axis {
    /// The horizontal axis: —
    Horizontal,
    /// The vertical axis: |
    Vertical,
}

impl Axis {
    /// Splits the provided [`Rectangle`] on the current [`Axis`] with the
    /// given `ratio` and `spacing`.
    pub fn split(
        &self,
        rectangle: &Rectangle,
        ratio: f32,
        spacing: f32,
        min_size_a: f32,
        min_size_b: f32,
    ) -> (Rectangle, Rectangle, f32) {
        match self {
            Axis::Horizontal => {
                let height_top = (rectangle.height * ratio - spacing / 2.0)
                    .round()
                    .max(min_size_a)
                    .min(rectangle.height - min_size_b - spacing);

                let height_bottom =
                    (rectangle.height - height_top - spacing).max(min_size_b);

                let ratio = (height_top + spacing / 2.0) / rectangle.height;

                (
                    Rectangle {
                        height: height_top,
                        ..*rectangle
                    },
                    Rectangle {
                        y: rectangle.y + height_top + spacing,
                        height: height_bottom,
                        ..*rectangle
                    },
                    ratio,
                )
            }
            Axis::Vertical => {
                let width_left = (rectangle.width * ratio - spacing / 2.0)
                    .round()
                    .max(min_size_a)
                    .min(rectangle.width - min_size_b - spacing);

                let width_right =
                    (rectangle.width - width_left - spacing).max(min_size_b);

                let ratio = (width_left + spacing / 2.0) / rectangle.width;

                (
                    Rectangle {
                        width: width_left,
                        ..*rectangle
                    },
                    Rectangle {
                        x: rectangle.x + width_left + spacing,
                        width: width_right,
                        ..*rectangle
                    },
                    ratio,
                )
            }
        }
    }

    /// Calculates the bounds of the split line in a [`Rectangle`] region.
    pub fn split_line_bounds(
        &self,
        rectangle: Rectangle,
        ratio: f32,
        spacing: f32,
    ) -> Rectangle {
        match self {
            Axis::Horizontal => Rectangle {
                x: rectangle.x,
                y: (rectangle.y + rectangle.height * ratio - spacing / 2.0)
                    .round(),
                width: rectangle.width,
                height: spacing,
            },
            Axis::Vertical => Rectangle {
                x: (rectangle.x + rectangle.width * ratio - spacing / 2.0)
                    .round(),
                y: rectangle.y,
                width: spacing,
                height: rectangle.height,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/pane_grid/axis.rs"
    ));
}
