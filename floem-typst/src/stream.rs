//! Streaming Typst compiler with frozen-block strategy.
//!
//! Accumulates Typst source chunks, detects block boundaries,
//! compiles completed blocks once (freeze), and only recompiles
//! the active tail on each debounced tick.

use std::time::{Duration, Instant};

use typst::diag::Warned;
use typst_library::layout::Frame;
use typst_layout::PagedDocument;

use crate::world::StreamWorld;

/// Debounce interval for recompilation (same as crow-ade's 80ms).
const TICK_INTERVAL: Duration = Duration::from_millis(80);

/// Line height (in points) used when drawing raw-text fallback for blocks
/// that fail to compile. Must match `render::FALLBACK_LINE_HEIGHT`.
pub const FALLBACK_LINE_HEIGHT: f64 = 18.0;

/// Number of consecutive error ticks before we give up on Typst for the
/// active tail and draw raw text instead. Guards against transient errors
/// from incomplete streamed prefixes (e.g. an unclosed `$`).
const ERROR_STREAK_THRESHOLD: u32 = 3;

/// Height of a raw-text fallback block, computed from line count so that
/// the modelled height exactly matches what `render::paint_fallback_text`
/// draws (no wrapping → no overlap with following blocks).
pub fn fallback_text_height(text: &str) -> f64 {
    if text.is_empty() {
        0.0
    } else {
        (text.matches('\n').count() + 1) as f64 * FALLBACK_LINE_HEIGHT
    }
}

/// A compiled and frozen block — its glyphs never change.
#[derive(Clone)]
pub struct FrozenBlock {
    /// The source range this block covers (byte offsets).
    pub source_range: (usize, usize),
    /// The compiled frame (positioned glyphs, shapes, images).
    pub frame: Frame,
    /// Vertical offset where this block starts in the document.
    pub y_offset: f64,
}

/// The active (incomplete) tail that gets recompiled each tick.
pub struct ActiveTail {
    /// Source text of the active tail.
    pub source: String,
    /// Last compiled frame for the tail (None if compilation failed).
    pub frame: Option<Frame>,
    /// Vertical offset where the tail starts.
    pub y_offset: f64,
}

/// Streaming Typst compiler.
pub struct TypstStream {
    /// Full accumulated source text.
    source: String,
    /// Byte offset where the active tail starts.
    tail_start: usize,
    /// Frozen (completed) blocks — append-only, never modified.
    frozen: Vec<FrozenBlock>,
    /// Current active tail state.
    active: ActiveTail,
    /// Last tick time (for debouncing).
    last_tick: Instant,
    /// The Typst world (fonts, packages).
    world: StreamWorld,
    /// Total height of frozen blocks (for layout).
    frozen_height: f64,
    /// Whether new content has been pushed since last tick.
    dirty: bool,
    /// Consecutive ticks where the active tail failed to compile.
    error_streak: u32,
    /// One-shot flag (set by `flush`) forcing raw-text fallback on the
    /// final tick so a persistently-erroring tail still shows its text.
    force_fallback: bool,
}

/// The renderable scene produced by a tick.
pub struct Scene {
    /// Frozen blocks (already rendered, just position them).
    pub frozen: Vec<FrozenBlock>,
    /// Active tail (re-render this each tick).
    pub active: ActiveTail,
    /// Total document height (frozen + active).
    pub total_height: f64,
    /// Whether anything changed since last tick.
    pub changed: bool,
    /// If true, the active tail should be drawn as raw text (it failed to
    /// compile persistently). `active.frame` will be `None` in this case.
    pub fallback_active: bool,
}

impl TypstStream {
    pub fn new() -> Self {
        Self {
            source: String::new(),
            tail_start: 0,
            frozen: Vec::new(),
            active: ActiveTail {
                source: String::new(),
                frame: None,
                y_offset: 0.0,
            },
            last_tick: Instant::now(),
            world: StreamWorld::new(),
            frozen_height: 0.0,
            dirty: false,
            error_streak: 0,
            force_fallback: false,
        }
    }

    /// Push a new chunk of Typst source.
    pub fn push(&mut self, chunk: &str) {
        self.source.push_str(chunk);
        self.dirty = true;
    }

    /// Check if it's time to tick (debounce).
    pub fn should_tick(&self) -> bool {
        self.dirty && self.last_tick.elapsed() >= TICK_INTERVAL
    }

    /// Force a tick regardless of debounce (e.g., on turn end).
    pub fn flush(&mut self) -> Scene {
        self.dirty = true;
        self.force_fallback = true;
        self.tick()
    }

    /// Process accumulated source: freeze completed blocks, recompile tail.
    pub fn tick(&mut self) -> Scene {
        self.last_tick = Instant::now();
        let force_fallback = self.force_fallback;
        self.force_fallback = false;

        if !self.dirty {
            let fallback_active = self.active.frame.is_none()
                && !self.active.source.is_empty()
                && (force_fallback || self.error_streak >= ERROR_STREAK_THRESHOLD);
            let active_h = if let Some(ref f) = self.active.frame {
                frame_height(f)
            } else if fallback_active {
                fallback_text_height(&self.active.source)
            } else {
                0.0
            };
            return Scene {
                frozen: self.frozen.clone(),
                active: ActiveTail {
                    source: self.active.source.clone(),
                    frame: self.active.frame.clone(),
                    y_offset: self.active.y_offset,
                },
                total_height: self.frozen_height + active_h,
                changed: false,
                fallback_active,
            };
        }
        self.dirty = false;

        // Find safe block boundaries in the source since tail_start.
        let boundary = self.find_block_boundary();

        if boundary > self.tail_start {
            // Clone the block source to avoid borrow conflict with compile_block.
            let block_source = self.source[self.tail_start..boundary].to_string();
            if let Some(frame) = self.compile_block(&block_source) {
                let height = frame_height(&frame);
                self.frozen.push(FrozenBlock {
                    source_range: (self.tail_start, boundary),
                    frame,
                    y_offset: self.frozen_height,
                });
                self.frozen_height += height;
                self.tail_start = boundary;
            } else {
                // A completed block failed to compile (e.g. a markdown `#`
                // heading, which is a Typst code expression). Do NOT advance
                // `tail_start`: leave the offending block in the active tail
                // so it keeps showing as raw-text fallback instead of
                // silently vanishing once frozen. Stop freezing for the rest
                // of this tick — the whole remainder becomes the tail.
            }
        }

        // Recompile the active tail.
        let tail_source = self.source[self.tail_start..].to_string();
        self.active.source = tail_source.clone();
        self.active.y_offset = self.frozen_height;
        self.active.frame = if tail_source.is_empty() {
            None
        } else {
            self.compile_block(&tail_source)
        };

        // Track persistent compile failures so we can fall back to raw text
        // without flickering on transiently-invalid streamed prefixes.
        if self.active.frame.is_none() && !tail_source.is_empty() {
            self.error_streak = self.error_streak.saturating_add(1);
        } else if self.active.frame.is_some() {
            self.error_streak = 0;
        }

        let fallback_active = self.active.frame.is_none()
            && !tail_source.is_empty()
            && (force_fallback || self.error_streak >= ERROR_STREAK_THRESHOLD);

        let active_h = if let Some(ref f) = self.active.frame {
            frame_height(f)
        } else if fallback_active {
            fallback_text_height(&tail_source)
        } else {
            0.0
        };

        Scene {
            frozen: self.frozen.clone(),
            active: ActiveTail {
                source: self.active.source.clone(),
                frame: self.active.frame.clone(),
                y_offset: self.active.y_offset,
            },
            total_height: self.frozen_height + active_h,
            changed: true,
            fallback_active,
        }
    }

    /// Find the last safe block boundary in the source.
    ///
    /// A safe boundary is a double-newline (\n\n) that is:
    /// - Not inside an open code fence (odd count of ```)
    /// - Not inside a math block ($ ... $)
    /// - Not at the very end (might get more content)
    fn find_block_boundary(&self) -> usize {
        let text = &self.source[self.tail_start..];
        let mut last_boundary = 0;
        let mut fence_count = 0;
        let mut in_math = false;
        let mut i = 0;
        let bytes = text.as_bytes();

        while i < bytes.len() {
            if i + 2 < bytes.len() && &bytes[i..i + 3] == b"```" {
                fence_count += 1;
                i += 3;
                continue;
            }

            if bytes[i] == b'$' {
                in_math = !in_math;
            }

            if i + 1 < bytes.len()
                && bytes[i] == b'\n'
                && bytes[i + 1] == b'\n'
                && fence_count % 2 == 0
                && !in_math
            {
                last_boundary = self.tail_start + i + 2;
                i += 2;
                continue;
            }

            i += 1;
        }

        last_boundary
    }

    /// Compile a block of Typst source into a Frame.
    fn compile_block(&mut self, source: &str) -> Option<Frame> {
        let wrapped = format!(
            "#set page(width: auto, height: auto, margin: 0pt)\n{}",
            source
        );

        self.world.set_source(&wrapped);

        match typst::compile::<PagedDocument>(&self.world) {
            Warned { output: Ok(doc), .. } => {
                doc.pages().first().map(|p| p.frame.clone())
            }
            Warned { output: Err(_), warnings } => {
                for w in warnings {
                    tracing::warn!("Typst warning: {:?}", w);
                }
                None
            }
        }
    }

    /// Get the full source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Reset the stream (new message).
    pub fn reset(&mut self) {
        self.source.clear();
        self.tail_start = 0;
        self.frozen.clear();
        self.active = ActiveTail {
            source: String::new(),
            frame: None,
            y_offset: 0.0,
        };
        self.frozen_height = 0.0;
        self.dirty = false;
        self.error_streak = 0;
        self.force_fallback = false;
    }
}

/// Get the height of a frame in points.
pub fn frame_height(frame: &Frame) -> f64 {
    frame.size().y.to_pt()
}

/// Get the width of a frame in points.
pub fn frame_width(frame: &Frame) -> f64 {
    frame.size().x.to_pt()
}
