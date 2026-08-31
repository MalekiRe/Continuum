use super::{MODEL_CONTEXT_LIMIT, ModelRequest};
use crate::kernel::{Kernel, SpineNode, TranscriptEntry};
use std::fmt::Write as _;

const DIRECTIVE: &str = "\nEmit exactly one Lisp form. No prose, tags, or Markdown. Use (begin ...) only for synchronous Lisp operations. bash, model/call, agent/call, agent/return, human/wait, and message/reply must be the final action of an evaluation.\n";

struct Section {
    heading: &'static str,
    body: String,
    budget: usize,
}

struct ContextBuilder {
    text: String,
    limit: usize,
}

impl ContextBuilder {
    fn new(limit: usize) -> Self {
        Self {
            text: String::with_capacity(limit),
            limit,
        }
    }

    fn push(&mut self, section: Section) {
        if section.body.is_empty() {
            return;
        }
        let heading = format!("\n# {}\n", section.heading);
        let remaining = self.limit.saturating_sub(self.text.len());
        if heading.len() >= remaining {
            return;
        }
        self.text.push_str(&heading);
        let remaining = self.limit.saturating_sub(self.text.len());
        self.text
            .push_str(&truncate(&section.body, section.budget.min(remaining)));
        if self.text.len() < self.limit && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn finish(mut self) -> String {
        self.text.push_str(DIRECTIVE);
        self.text
    }
}

pub(super) fn build_request(kernel: &Kernel) -> (ModelRequest, Option<u64>) {
    let frame = kernel
        .frames
        .last()
        .expect("build_request requires a frame");
    let system = if frame.state.instructions.is_empty() {
        "You are Continuum, a persistent agent inhabiting a Lisp world. Choose one useful Lisp action.".into()
    } else {
        truncate(&frame.state.instructions, 16_000)
    };
    let mut builder =
        ContextBuilder::new(MODEL_CONTEXT_LIMIT.saturating_sub(system.len() + DIRECTIVE.len()));
    let (notices, watermark) = render_notices(kernel, 12_000);
    let sections = [
        Section {
            heading: "Current human messages and notices",
            body: notices,
            budget: 12_000,
        },
        Section {
            heading: "Active frame stack",
            body: render_stack(kernel),
            budget: 4_000,
        },
        Section {
            heading: "Context injections and selected memory",
            body: render_guidance(kernel),
            budget: 12_000,
        },
        Section {
            heading: "Recent Lisp actions and results",
            body: render_transcript(&frame.state.transcript, 24_000),
            budget: 24_000,
        },
        Section {
            heading: "Chronological history spine",
            body: crate::kernel::history::render_spine(&frame.state.spine, 8_000),
            budget: 6_000,
        },
        Section {
            heading: "Library discovery",
            body: render_library(kernel),
            budget: 4_000,
        },
    ];
    for section in sections {
        builder.push(section);
    }
    (
        ModelRequest {
            system,
            context: builder.finish(),
        },
        watermark,
    )
}

fn render_notices(kernel: &Kernel, budget: usize) -> (String, Option<u64>) {
    let frame = kernel.frames.last().expect("active frame");
    let mut output = String::new();
    let mut watermark = None;
    for notice in kernel.notices_for_frame(&frame.id) {
        let heading = match (&notice.id, notice.handled) {
            (Some(id), false) => format!("- Human message [{id}]: "),
            (Some(id), true) => format!(
                "- Answered human notice [{id}] (informational; do not call message/reply): "
            ),
            (None, _) => "- ".into(),
        };
        let remaining = budget.saturating_sub(output.len());
        if remaining <= heading.len() + 1 {
            break;
        }
        let body = truncate(&notice.text, remaining - heading.len() - 1);
        let _ = writeln!(output, "{heading}{body}");
        watermark = Some(notice.sequence);
    }
    (output, watermark)
}
fn render_stack(kernel: &Kernel) -> String {
    let mut output = String::new();
    let active = kernel.frames.len().saturating_sub(1);
    for (index, frame) in kernel.frames.iter().enumerate() {
        let state = if index != active {
            "blocked on child"
        } else if frame.waiting_for_human {
            "waiting for human"
        } else {
            "active"
        };
        let _ = writeln!(output, "- {} [{}] {state}", frame.name, frame.id);
    }
    output
}

fn render_guidance(kernel: &Kernel) -> String {
    let frame = kernel.frames.last().expect("active frame");
    let mut output = String::new();
    for entry in &frame.state.context_entries {
        let _ = writeln!(
            output,
            "{} [{:?}]: {}",
            truncate(&entry.key, 200),
            entry.lifetime,
            truncate(&entry.text, 2_000)
        );
    }
    if let Some(root) = kernel.frames.first() {
        output.push_str(&crate::kernel::history::render_memory(
            &root.state.memory,
            &root.state.memory_index,
            8_000,
        ));
    }
    if kernel.frames.len() > 1 {
        output.push_str(&crate::kernel::history::render_memory(
            &frame.state.memory,
            &frame.state.memory_index,
            4_000,
        ));
    }
    output
}

fn render_library(kernel: &Kernel) -> String {
    let mut output = String::new();
    for namespace in kernel.env.namespace_names() {
        let bindings = kernel.inspect_namespace(&namespace).unwrap_or_default();
        let _ = writeln!(output, "{namespace}: {}", bindings.join(", "));
    }
    let _ = writeln!(output, "\nDefinitions with retained source:");
    for (namespace, values) in kernel.env.namespaces.iter() {
        let mut names: Vec<_> = values.sources.keys().collect();
        names.sort();
        for name in names {
            let _ = writeln!(output, "- {namespace}/{name}");
        }
    }
    output
}

fn render_recent<'a, T: 'a>(
    items: impl DoubleEndedIterator<Item = &'a T>,
    budget: usize,
    render: impl Fn(&T) -> String,
    keep_partial: bool,
) -> (String, usize) {
    let mut selected = Vec::new();
    let mut used = 0;
    for item in items.rev() {
        let item = render(item);
        if used + item.len() > budget {
            if keep_partial && selected.is_empty() {
                selected.push(truncate(&item, budget));
            }
            break;
        }
        used += item.len();
        selected.push(item);
    }
    selected.reverse();
    let count = selected.len();
    (selected.concat(), count)
}

fn render_transcript(entries: &[TranscriptEntry], budget: usize) -> String {
    render_recent(
        entries.iter(),
        budget,
        |entry| {
            format!(
                "> {}\n{}\n",
                truncate(&entry.source, 600),
                truncate(&entry.result, 1_200)
            )
        },
        true,
    )
    .0
}

pub(super) fn compact_current_frame(kernel: &mut Kernel) {
    const RECENT_BUDGET: usize = 24_000;
    let Some(frame) = kernel.frames.last_mut() else {
        return;
    };
    let size = |entry: &TranscriptEntry| entry.source.len() + entry.result.len() + 8;
    let mut recent: usize = frame.state.transcript.iter().map(size).sum();
    while recent > RECENT_BUDGET && frame.state.transcript.len() > 1 {
        let entry = frame.state.transcript.remove(0);
        recent = recent.saturating_sub(size(&entry));
        crate::kernel::history::push_spine(
            &mut frame.state.spine,
            SpineNode {
                level: 0,
                first_event: entry.event_id,
                last_event: entry.event_id,
                summary: truncate(&format!("{} => {}", entry.source, entry.result), 720),
            },
        );
    }
}
pub(super) fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.into();
    }
    const ELLIPSIS: &str = "…";
    let suffix = if max >= ELLIPSIS.len() { ELLIPSIS } else { "" };
    let limit = max.saturating_sub(suffix.len()).min(value.len());
    let end = (0..=limit)
        .rfind(|&index| value.is_char_boundary(index))
        .unwrap_or_default();
    format!("{}{suffix}", &value[..end])
}
