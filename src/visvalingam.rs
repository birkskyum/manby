//! Port of visvalingam.cpp.
//!
//! The C++ links the heap items with raw pointers. Here the items live in one
//! vector and refer to each other by index.

// Adapted from
// https://github.com/paulmach/orb/blob/dcade4901baea0727377ccf7c4aab2addd92d152/simplify/visvalingam.go

// The MIT License (MIT)
//
// Copyright (c) 2017 Paul Mach
//
// Permission is hereby granted, free of charge, to any person obtaining
// a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject
// to the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR
// ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
// CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
// WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use crate::geometry::DrawVec;

// Stuff to create the priority queue, or min heap.
// Rewriting it here, vs using the std lib, resulted in a 50% performance bump!

#[derive(Clone, Copy, Debug, Default)]
struct VisItem {
    area: f64,        // triangle area
    point_index: i32, // index of point in original path

    // to keep a virtual linked list to help rebuild the triangle areas as we remove points.
    next: Option<usize>,
    previous: Option<usize>,

    index: i32, // interal index in heap, for removal and update
}

struct MinHeap {
    h: Vec<usize>,
}

impl MinHeap {
    fn push(&mut self, items: &mut [VisItem], item: usize) {
        items[item].index = self.h.len() as i32;
        self.h.push(item);
        self.up(items, items[item].index);
    }

    fn pop(&mut self, items: &mut [VisItem]) -> usize {
        let removed = self.h[0];
        let last_item = self.h[self.h.len() - 1];
        self.h.pop();

        if !self.h.is_empty() {
            items[last_item].index = 0;
            self.h[0] = last_item;
            self.down(items, 0);
        }

        removed
    }

    fn update(&mut self, items: &mut [VisItem], item: usize, area: f64) {
        if items[item].area > area {
            // area got smaller
            items[item].area = area;
            self.up(items, items[item].index);
        } else {
            // area got larger
            items[item].area = area;
            self.down(items, items[item].index);
        }
    }

    fn up(&mut self, items: &mut [VisItem], mut i: i32) {
        let object = self.h[i as usize];
        while i > 0 {
            let up = ((i + 1) >> 1) - 1;
            let parent = self.h[up as usize];

            if items[parent].area <= items[object].area {
                // parent is smaller so we're done fixing up the heap.
                break;
            }

            // swap nodes
            items[parent].index = i;
            self.h[i as usize] = parent;

            items[object].index = up;
            self.h[up as usize] = object;

            i = up;
        }
    }

    fn down(&mut self, items: &mut [VisItem], mut i: i32) {
        let object = self.h[i as usize];
        loop {
            let right = ((i + 1) << 1) as usize;
            let left = right.wrapping_sub(1);

            let mut down = i;
            let mut child = self.h[down as usize];

            // swap with smallest child
            if left < self.h.len() && items[self.h[left]].area < items[child].area {
                down = left as i32;
                child = self.h[down as usize];
            }

            if right < self.h.len() && items[self.h[right]].area < items[child].area {
                down = right as i32;
                child = self.h[down as usize];
            }

            // non smaller, so quit
            if down == i {
                break;
            }

            // swap the nodes
            items[child].index = i;
            self.h[items[child].index as usize] = child;

            items[object].index = down;
            self.h[down as usize] = object;

            i = down;
        }
    }
}

fn double_triangle_area(ls: &DrawVec, start: usize, i1: i32, i2: i32, i3: i32) -> f64 {
    let a = ls[i1 as usize + start];
    let b = ls[i2 as usize + start];
    let c = ls[i3 as usize + start];

    (b.x - a.x)
        .wrapping_mul(c.y - a.y)
        .wrapping_sub((b.y - a.y).wrapping_mul(c.x - a.x))
        .wrapping_abs() as f64
}

/// `std::max(a, b)`, which returns `a` unless `a < b`.
fn std_max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

pub fn visvalingam(ls: &mut DrawVec, start: usize, end: usize, threshold: f64, retain: usize) {
    // edge cases checked, get on with it
    let mut removed: i32 = 0;
    let threshold = threshold * 2.0;

    // build the initial minheap linked list.
    let mut heap = MinHeap { h: Vec::new() };

    // internal path items. The C++ keeps linkedListStart outside the vector;
    // here it is the extra item at the end.
    let mut items: Vec<VisItem> = vec![VisItem::default(); end - start];
    let linked_list_start = items.len();
    items.push(VisItem {
        area: f64::INFINITY,
        point_index: 0,
        ..Default::default()
    });
    heap.push(&mut items, linked_list_start);

    {
        let mut previous = linked_list_start;
        for i in 1..(end - start) - 1 {
            items[i].area = double_triangle_area(ls, start, i as i32 - 1, i as i32, i as i32 + 1);
            items[i].point_index = i as i32;
            items[i].previous = Some(previous);

            heap.push(&mut items, i);
            items[previous].next = Some(i);
            previous = i;
        }

        // final item
        let end_item = (end - start) - 1;
        items[end_item].area = f64::INFINITY;
        items[end_item].point_index = ((end - start) - 1) as i32;
        items[end_item].previous = Some(previous);

        items[previous].next = Some(end_item);
        heap.push(&mut items, end_item);
    }

    // run through the reduction process
    while !heap.h.is_empty() {
        let current = heap.pop(&mut items);
        if items[current].area > threshold || (end - start).wrapping_sub(removed as usize) <= retain
        {
            break;
        }

        let next = items[current].next.unwrap();
        let previous = items[current].previous.unwrap();

        // remove current element from linked list
        items[previous].next = items[current].next;
        items[next].previous = items[current].previous;
        removed += 1;

        // figure out the new areas
        if let Some(pp) = items[previous].previous {
            let mut area = double_triangle_area(
                ls,
                start,
                items[pp].point_index,
                items[previous].point_index,
                items[next].point_index,
            );

            area = std_max(area, items[current].area);
            heap.update(&mut items, previous, area);
        }

        if let Some(nn) = items[next].next {
            let mut area = double_triangle_area(
                ls,
                start,
                items[previous].point_index,
                items[next].point_index,
                items[nn].point_index,
            );

            area = std_max(area, items[current].area);
            heap.update(&mut items, next, area);
        }
    }

    let mut item = Some(linked_list_start);
    while let Some(it) = item {
        ls[items[it].point_index as usize + start].necessary = 1;
        item = items[it].next;
    }
}
