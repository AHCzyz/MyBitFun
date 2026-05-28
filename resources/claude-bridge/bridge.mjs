#!/usr/bin/env node
/**
 * Claude Agent SDK → BitFun JSONL bridge.
 *
 * Reads JSONL commands from stdin, writes JSONL events to stdout.
 * Uses @anthropic-ai/claude-agent-sdk for agent execution.
 *
 * Command format (stdin, one JSON object per line):
 *   {"command":"prompt","text":"...","model":"...","workingDir":"..."}
 *   {"command":"abort"}
 *
 * Event format (stdout, one JSON object per line):
 *   {"type":"text_delta","delta":"..."}
 *   {"type":"thinking_delta","delta":"..."}
 *   {"type":"tool_call_start","tool_call_id":"...","tool_name":"..."}
 *   {"type":"tool_call_delta","tool_call_id":"...","delta":"..."}
 *   {"type":"tool_result","tool_call_id":"...","result":"..."}
 *   {"type":"turn_end","stopReason":"completed"}
 *   {"type":"error","message":"..."}
 */

import { query } from '@anthropic-ai/claude-agent-sdk';
import { createInterface } from 'node:readline';

// ── Message translation ─────────────────────────────────────────────────────

/**
 * Translate a Claude SDK message into one or more BitFun JSONL events.
 * Returns an array of event objects (may be empty if the message type is unhandled).
 */
function translateMessage(msg) {
  const events = [];

  // Case 1: stream_event — the SDK emits streaming content deltas
  if (msg.type === 'stream_event' && msg.event) {
    const ev = msg.event;
    switch (ev.type) {
      case 'content_block_start': {
        const block = ev.content_block || ev.index != null ? ev : null;
        if (block?.content_block?.type === 'tool_use') {
          const tu = block.content_block;
          events.push({
            type: 'tool_call_start',
            tool_call_id: tu.id ?? '',
            tool_name: tu.name ?? '',
          });
        }
        break;
      }
      case 'content_block_delta': {
        const delta = ev.delta;
        if (!delta) break;
        if (delta.type === 'text_delta') {
          events.push({ type: 'text_delta', delta: delta.text ?? '' });
        } else if (delta.type === 'input_json_delta') {
          // Tool call argument streaming — emit as tool_call_delta
          events.push({
            type: 'tool_call_delta',
            tool_call_id: ev.index != null ? String(ev.index) : '',
            delta: delta.partial_json ?? '',
          });
        } else if (delta.type === 'thinking_delta') {
          events.push({
            type: 'thinking_delta',
            delta: delta.thinking ?? '',
          });
        } else if (delta.type === 'signature_delta') {
          // signature deltas are internal; skip
        }
        break;
      }
      case 'content_block_stop': {
        // End of a content block; no event to emit
        break;
      }
      default:
        // Unknown stream event — log to stderr but don't fail
        break;
    }
    return events;
  }

  // Case 2: Full assistant message (non-streaming or final)
  if (msg.type === 'assistant' && msg.message?.content) {
    for (const block of msg.message.content) {
      switch (block.type) {
        case 'text':
          events.push({ type: 'text_delta', delta: block.text ?? '' });
          break;
        case 'tool_use':
          events.push({
            type: 'tool_call_start',
            tool_call_id: block.id ?? '',
            tool_name: block.name ?? '',
          });
          if (block.input) {
            events.push({
              type: 'tool_call_delta',
              tool_call_id: block.id ?? '',
              delta: JSON.stringify(block.input),
            });
          }
          break;
        case 'thinking':
          events.push({
            type: 'thinking_delta',
            delta: block.thinking ?? '',
          });
          break;
        default:
          break;
      }
    }
    return events;
  }

  // Case 3: User message (tool results come back as user messages with tool_result blocks)
  if (msg.type === 'user' && msg.message?.content) {
    for (const block of msg.message.content) {
      if (block.type === 'tool_result') {
        const resultText =
          typeof block.content === 'string'
            ? block.content
            : Array.isArray(block.content)
              ? block.content.map(c => c.text ?? '').join('')
              : JSON.stringify(block.content ?? '');
        events.push({
          type: 'tool_result',
          tool_call_id: block.tool_use_id ?? '',
          result: resultText,
        });
      }
    }
    return events;
  }

  // Case 4: Result message (end of query)
  if (msg.type === 'result' || msg.subtype === 'success' || msg.result !== undefined) {
    // This is emitted by the bridge after the loop, not here
    return events;
  }

  // Case 5: Error message
  if (msg.type === 'error') {
    events.push({
      type: 'error',
      message: msg.message ?? msg.error ?? 'Unknown SDK error',
    });
    return events;
  }

  // Unknown message shape — log for debugging, but skip
  return events;
}

// ── Main loop ────────────────────────────────────────────────────────────────

async function main() {
  const rl = createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  // Process commands line by line
  for await (const line of rl) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    let cmd;
    try {
      cmd = JSON.parse(trimmed);
    } catch {
      process.stderr.write(`bridge: invalid JSON: ${trimmed}\n`);
      continue;
    }

    if (cmd.command === 'abort') {
      process.exit(0);
    }

    if (cmd.command !== 'prompt') {
      process.stderr.write(`bridge: unknown command: ${cmd.command}\n`);
      continue;
    }

    // Build options for the Claude SDK
    const options = {};
    if (cmd.model) options.model = cmd.model;
    if (cmd.workingDir) options.workingDir = cmd.workingDir;

    try {
      const messages = query({
        prompt: cmd.text,
        options,
      });

      for await (const msg of messages) {
        const events = translateMessage(msg);
        for (const ev of events) {
          process.stdout.write(JSON.stringify(ev) + '\n');
        }
      }

      // Turn completed successfully
      process.stdout.write(
        JSON.stringify({ type: 'turn_end', stopReason: 'completed' }) + '\n',
      );
    } catch (err) {
      // Report error and end turn with error status
      process.stdout.write(
        JSON.stringify({
          type: 'error',
          message: err.message ?? String(err),
        }) + '\n',
      );
      process.stdout.write(
        JSON.stringify({ type: 'turn_end', stopReason: 'error' }) + '\n',
      );
    }
  }
}

main().catch((err) => {
  process.stderr.write(`bridge fatal: ${err.message}\n`);
  process.exit(1);
});
