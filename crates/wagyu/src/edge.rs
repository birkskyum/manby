//! Port of `edge.hpp`.

use crate::geometry::Point;
use crate::util::value_is_zero;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Edge {
    pub bot: Point,
    pub top: Point,
    pub dx: f64,
}

impl Edge {
    pub fn new(current: Point, next_pt: Point) -> Edge {
        let mut bot = current;
        let mut top = current;
        if current.y >= next_pt.y {
            top = next_pt;
        } else {
            bot = next_pt;
        }
        let dy = (top.y - bot.y) as f64;
        let dx = if value_is_zero(dy) {
            f64::INFINITY
        } else {
            (top.x - bot.x) as f64 / dy
        };
        Edge { bot, top, dx }
    }
}

// PORT: wrapping products, see `util::slopes_equal`.
pub(crate) fn slopes_equal_edges(e1: &Edge, e2: &Edge) -> bool {
    (e1.top.y - e1.bot.y).wrapping_mul(e2.top.x - e2.bot.x)
        == (e1.top.x - e1.bot.x).wrapping_mul(e2.top.y - e2.bot.y)
}

#[inline]
pub(crate) fn is_horizontal(e: &Edge) -> bool {
    e.dx.is_infinite()
}

#[inline]
pub(crate) fn get_current_x(edge: &Edge, current_y: i64) -> f64 {
    if current_y == edge.top.y {
        edge.top.x as f64
    } else {
        edge.bot.x as f64 + edge.dx * (current_y - edge.bot.y) as f64
    }
}
