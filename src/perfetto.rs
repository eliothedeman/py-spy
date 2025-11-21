use std::cmp::min;
use std::collections::HashMap;
use std::io::Write;

use anyhow::Error;
use bytes::BufMut;
use bytes::BytesMut;
use perfetto_writer::Context;

use crate::stack_trace::Frame;
use crate::stack_trace::StackTrace;

pub struct PerfettoTrace {
    encoder: Context,
    thread_to_track: HashMap<(i32, u64), u64>,
    prev_traces: HashMap<u64, StackTrace>,
    show_linenumbers: bool,
}

impl PerfettoTrace {
    pub fn new(show_linenumbers: bool) -> Self {
        Self {
            encoder: Context::new(),
            show_linenumbers,
            thread_to_track: HashMap::new(),
            prev_traces: HashMap::new(),
        }
    }

    fn track_for(&mut self, trace: &StackTrace) -> u64 {
        if let Some(id) = self.thread_to_track.get(&(trace.pid, trace.thread_id)) {
            return *id;
        }

        let id = self
            .encoder
            .track()
            .pid(trace.pid)
            .tid(trace.thread_id as i32)
            .uuid(rand::random())
            .build();

        self.thread_to_track
            .insert((trace.pid, trace.thread_id), id);
        id
    }

    // Return whether these frames are similar enough such that we should merge
    // them, instead of creating separate events for them.
    fn should_merge_frames(&self, a: &Frame, b: &Frame) -> bool {
        a.name == b.name && a.filename == b.filename && (!self.show_linenumbers || a.line == b.line)
    }

    pub fn increment(&mut self, trace: &StackTrace) -> std::io::Result<()> {
        let track = self.track_for(trace);

        let prev_owned_gil = self
            .prev_traces
            .get(&trace.thread_id)
            .map(|stack| stack.owns_gil)
            .unwrap_or(false);

        // Load the previous frames for this thread.
        let prev_frames = self
            .prev_traces
            .remove(&trace.thread_id)
            .map(|t| t.frames)
            .unwrap_or_default();

        // Find the index where we first see new frames.
        let new_idx = prev_frames
            .iter()
            .rev()
            .zip(trace.frames.iter().rev())
            .position(|(a, b)| {
                !(self.should_merge_frames(a, b) && prev_owned_gil == trace.owns_gil)
            })
            .unwrap_or(min(prev_frames.len(), trace.frames.len()));

        // Publish end events for the previous frames that got dropped in the
        // most recent trace.
        for frame in prev_frames.iter().rev().skip(new_idx).rev() {
            self.encoder
                .event()
                .with_track_uuid(track)
                .with_end()
                .with_now()
                .with_name(&frame.name)
                .with_source_location(&frame.filename, frame.line as u32)
                .build();
        }

        // Publish start events for frames that got added in the most recent
        // trace.
        for frame in trace.frames.iter().rev().skip(new_idx) {
            self.encoder
                .event()
                .with_track_uuid(track)
                .with_begin()
                .with_debug_bool("owns_gil", trace.owns_gil)
                .with_now()
                .with_name(&frame.name)
                .with_source_location(&frame.filename, frame.line as u32)
                .build();
        }

        // Save this stack trace for the next iteration.
        self.prev_traces.insert(trace.thread_id, trace.clone());

        Ok(())
    }

    pub fn write(&mut self, w: &mut dyn Write) -> Result<(), Error> {
        for (_, trace) in self.prev_traces.iter() {
            let track = self
                .thread_to_track
                .get(&(trace.pid, trace.thread_id))
                .unwrap();
            for frame in trace.frames.iter() {
                self.encoder
                    .event()
                    .with_track_uuid(*track)
                    .with_end()
                    .with_now()
                    .with_name(&frame.name)
                    .with_source_location(&frame.filename, frame.line as u32)
                    .build();
            }
        }
        self.encoder.write_to(w)?;
        Ok(())
    }
}
