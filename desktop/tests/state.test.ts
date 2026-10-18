import { test } from 'node:test';
import assert from 'node:assert/strict';
import { JobState, selectedEntries, category } from '../src/state.ts';

const entry = { id: '18446744073709551614', name: 'photo.png', format: 'png', bytes: 28,
  recoverable: true, inspection: 'checksum_verified', warnings: [], selection: '{"offset":18446744073709551614}' };

test('stale jobs cannot add candidates or finish a running job', () => {
  const state = new JobState(); state.begin('new', 'scan');
  state.accept({ job_id: 'old', payload: { event: 'candidate', entry } });
  state.accept({ job_id: 'old', payload: { event: 'terminal', success: true, cancelled: false, error: null } });
  assert.equal(state.entries.length, 0); assert.equal(state.status, 'running');
});
test('selection tokens stay opaque and recovery includes only selected recoverable entries', () => {
  const state = new JobState(); state.begin('scan', 'scan');
  state.accept({ job_id: 'scan', payload: { event: 'candidate', entry } });
  state.accept({ job_id: 'scan', payload: { event: 'candidate', entry: { ...entry, id: 'other', selection: 'other', recoverable: false } } });
  assert.equal(state.entries[0].id, '18446744073709551614');
  assert.deepEqual(selectedEntries(state.entries, new Set([entry.selection, 'other'])), [entry.selection]);
});
test('complete is provisional until successful terminal and cancellation retains evidence', () => {
  const state = new JobState(); state.begin('a', 'recover');
  state.accept({ job_id: 'a', payload: { event: 'complete', directory: '/results/session', count: 1, failed: 0 } });
  assert.equal(state.status, 'running');
  state.accept({ job_id: 'a', payload: { event: 'terminal', success: false, cancelled: true, error: null } });
  assert.equal(state.status, 'cancelled'); assert.equal(state.directory, '/results/session');
});
test('nonzero terminal is failure and late candidate events are ignored', () => {
  const state = new JobState(); state.begin('a', 'scan');
  state.accept({ job_id: 'a', payload: { event: 'terminal', success: false, cancelled: false, error: 'bad protocol' } });
  state.accept({ job_id: 'a', payload: { event: 'candidate', entry } });
  assert.equal(state.status, 'failed'); assert.equal(state.error, 'bad protocol'); assert.equal(state.entries.length, 0);
});
test('category classification includes filesystem documents and arbitrary files', () => {
  assert.equal(category('png', 'x.png'), 'image'); assert.equal(category('unknown', 'notes.PDF'), 'document');
  assert.equal(category('unknown', 'archive.zip'), 'archive'); assert.equal(category('wav', 'sound.wav'), 'audio');
  assert.equal(category('unknown', 'data.bin'), 'other');
});
