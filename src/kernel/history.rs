use super::{MemoryEntry, MemoryNode, SpineNode};
use std::fmt::Write as _;

const FANOUT: usize = 8;
const RECENT_MEMORY: usize = 16;

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.into();
    }
    let mut end = max.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

pub(crate) fn push_spine(nodes: &mut Vec<SpineNode>, node: SpineNode) {
    nodes.push(node);
    loop {
        let Some(level) = nodes.last().map(|node| node.level) else {
            return;
        };
        let count = nodes
            .iter()
            .rev()
            .take_while(|node| node.level == level)
            .count();
        if count < FANOUT {
            return;
        }
        let first = nodes.len() - FANOUT;
        let merged: Vec<_> = nodes.drain(first..).collect();
        let summary = truncate(
            &merged
                .iter()
                .map(|node| node.summary.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            2_400,
        );
        nodes.push(SpineNode {
            level: level + 1,
            first_event: merged.first().expect("fanout").first_event,
            last_event: merged.last().expect("fanout").last_event,
            summary,
        });
    }
}

fn push_memory(nodes: &mut Vec<MemoryNode>, node: MemoryNode) {
    nodes.push(node);
    loop {
        let Some(level) = nodes.last().map(|node| node.level) else {
            return;
        };
        let count = nodes
            .iter()
            .rev()
            .take_while(|node| node.level == level)
            .count();
        if count < FANOUT {
            return;
        }
        let first = nodes.len() - FANOUT;
        let merged: Vec<_> = nodes.drain(first..).collect();
        let summary = truncate(
            &merged
                .iter()
                .map(|node| node.summary.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            2_400,
        );
        nodes.push(MemoryNode {
            level: level + 1,
            first: merged.first().expect("fanout").first.clone(),
            last: merged.last().expect("fanout").last.clone(),
            summary,
        });
    }
}

pub(crate) fn rebuild_memory_index(memory: &[MemoryEntry], index: &mut Vec<MemoryNode>) {
    index.clear();
    let sealed = memory.len().saturating_sub(RECENT_MEMORY);
    for entry in &memory[..sealed] {
        push_memory(
            index,
            MemoryNode {
                level: 0,
                first: entry.id.clone(),
                last: entry.id.clone(),
                summary: truncate(&format!("{}: {}", entry.key, entry.value), 600),
            },
        );
    }
}

pub(crate) fn render_memory(memory: &[MemoryEntry], index: &[MemoryNode], budget: usize) -> String {
    let mut output = String::new();
    for node in index {
        let line = format!(
            "[memory level {} {}..{}] {}\n",
            node.level, node.first, node.last, node.summary
        );
        if output.len() + line.len() > budget {
            break;
        }
        output.push_str(&line);
    }
    let start = memory.len().saturating_sub(RECENT_MEMORY);
    for entry in &memory[start..] {
        let line = format!("[{}] {}: {}\n", entry.id, entry.key, entry.value);
        if output.len() + line.len() > budget {
            break;
        }
        output.push_str(&line);
    }
    output
}

pub(crate) fn render_spine(nodes: &[SpineNode], budget: usize) -> String {
    let mut output = String::new();
    for node in nodes {
        let mut line = String::new();
        let _ = writeln!(
            line,
            "[history level {} events {}..{}] {}",
            node.level, node.first_event, node.last_event, node.summary
        );
        if output.len() + line.len() > budget {
            break;
        }
        output.push_str(&line);
    }
    output
}
