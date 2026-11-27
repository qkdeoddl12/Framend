import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { JobState, category, selectedEntries } from './state.ts';
import type { Category, Entry, Envelope, Request } from './state.ts';
import { messages } from './i18n.ts';
import type { Language, MessageKey } from './i18n.ts';
import './style.css';

const brandIcon = new URL('../src-tauri/icons/128x128.png', import.meta.url).href;
const state = new JobState();
let language: Language = localStorage.getItem('framend-language') === 'en' ? 'en' : 'ko';
let mode: 'recovery' | 'repair' = 'recovery';
let source = '', reference = '', output = '', ffmpeg = '', ffprobe = '';
let method: 'carve' | 'filesystem' = 'carve', filter: Category = 'all', search = '', page = 0;
let selected = new Set<string>(), ready = false, cancelling = false, renderTimer = 0, previewGeneration = 0;
let preview: { name: string; format: string; url?: string; error?: string } | null = null;
const logs: string[] = [];
const pageSize = 150;
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const t = (key: MessageKey): string => messages[language][key];
const esc = (text: unknown) => String(text ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]!));
const busy = () => state.status === 'running';
const size = (n: number) => {
  if (!Number.isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  const unit = Math.min(Math.floor(Math.log(n) / Math.log(1024)), 4);
  return `${(n / 1024 ** unit).toFixed(1)} ${['B','KiB','MiB','GiB','TiB'][unit]}`;
};
function inspection(value: unknown): string {
  if (typeof value === 'object' && value !== null) return Object.entries(value).map(([k, v]) => `${k}: ${typeof v === 'object' ? JSON.stringify(v) : v}`).join(' · ');
  const labels: Record<string, MessageKey> = { unverified: 'unverified', structure_only: 'structure', structure_checked: 'structure', structurally_valid: 'structure', structure_verified: 'structure', checksum_verified: 'checksum', decode_passed: 'decoded', decoded: 'decoded', failed: 'inspectionFailed' };
  return labels[String(value)] ? t(labels[String(value)]) : String(value || t('unverified'));
}
function warningText(warning: string): string {
  return warning === 'Structural candidate; content decoding has not been verified.' ? t('warningStructural') : warning;
}
function phaseText(phase: string): string {
  const labels: Record<string, MessageKey> = { reference_validation: 'phaseReferenceValidation', salvage: 'phaseSalvage', mux: 'phaseMux', scan: 'phaseScan', filesystem: 'phaseFilesystem', copy: 'phaseCopy' };
  return labels[phase] ? t(labels[phase]) : phase;
}
function log(message: string) {
  logs.push(`${new Date().toLocaleTimeString(language)}  ${message}`);
  if (logs.length > 100) logs.shift();
}
function pickRow(id: string, title: string, help: string, value: string, folder = false) {
  return `<div class="path-row"><div><label>${esc(title)}</label><p class="hint">${esc(help)}</p><p class="path" title="${esc(value)}">${esc(value || t(folder ? 'noFolder' : 'noFile'))}</p></div><button id="${id}" class="secondary" ${busy() ? 'disabled' : ''}>${t(folder ? 'chooseFolder' : 'choose')}</button></div>`;
}
function shell() {
  document.documentElement.lang = language;
  $('app').innerHTML = `
    <header><div class="brand"><img class="mark" src="${brandIcon}" alt="" width="42" height="42"><div><strong>Framend</strong><span>${t('tagline')}</span></div></div>
      <label class="language">${t('language')} <select id="language"><option value="ko" ${language === 'ko' ? 'selected' : ''}>한국어</option><option value="en" ${language === 'en' ? 'selected' : ''}>English</option></select></label></header>
    <main><div class="intro"><div><h1>${t(mode)}</h1><p>${t(mode === 'repair' ? 'repairIntro' : 'intro')}</p></div>
      <nav aria-label="Mode"><button id="recovery-mode" class="${mode === 'recovery' ? 'active' : ''}" ${busy() ? 'disabled' : ''}>${t('recovery')}</button><button id="repair-mode" class="${mode === 'repair' ? 'active' : ''}" ${busy() ? 'disabled' : ''}>${t('repair')}</button></nav></div>
    <section class="panel setup" aria-label="${t('source')}">
      ${pickRow('pick-source', t(mode === 'repair' ? 'damaged' : 'source'), t('sourceHelp'), source)}
      ${mode === 'repair' ? pickRow('pick-reference', t('reference'), t('referenceHelp'), reference) : ''}
      ${pickRow('pick-output', t('output'), t('outputHelp'), output, true)}
      ${mode === 'recovery' ? `<div class="method"><label for="method">${t('method')}</label><select id="method" ${busy() ? 'disabled' : ''}><option value="carve" ${method === 'carve' ? 'selected' : ''}>${t('carve')}</option><option value="filesystem" ${method === 'filesystem' ? 'selected' : ''}>${t('filesystem')}</option></select><p class="hint">${t(method === 'carve' ? 'carveHelp' : 'filesystemHelp')}</p></div>` : ''}
      <details class="tools"><summary>${t('tools')}</summary><p class="hint">${t('toolsHelp')}</p>${pickRow('pick-ffmpeg', t('ffmpeg'), '', ffmpeg)}${ffmpeg ? `<button id="clear-ffmpeg" class="text-button" ${busy() ? 'disabled' : ''}>${t('clear')}</button>` : ''}${pickRow('pick-ffprobe', t('ffprobe'), '', ffprobe)}${ffprobe ? `<button id="clear-ffprobe" class="text-button" ${busy() ? 'disabled' : ''}>${t('clear')}</button>` : ''}</details>
      <div class="actions"><button id="start" class="primary">${t(mode === 'repair' ? 'repairAction' : 'scan')}</button>${mode === 'recovery' ? `<button id="recover" class="secondary">${t('recover')} <span id="selected-count"></span></button>` : ''}<button id="cancel" class="danger">${t(cancelling ? 'cancelling' : 'cancel')}</button></div>
    </section>
    <section id="status" class="status" aria-live="polite"></section>
    ${mode === 'recovery' ? `<section class="panel candidates"><div class="section-top"><h2>${t('candidates')} <span id="found" class="count"></span></h2><input id="search" type="search" placeholder="${t('search')}" aria-label="${t('search')}" value="${esc(search)}"></div><div class="filters" role="group" aria-label="${t('format')}">${(['all','image','video','audio','document','archive','other'] as Category[]).map(c => `<button data-filter="${c}" class="${filter === c ? 'active' : ''}">${t(c)}</button>`).join('')}</div><div class="selection-actions"><button id="select-page" class="text-button">${t('selectPage')}</button><button id="clear-selection" class="text-button">${t('clearSelection')}</button><span id="selection-summary"></span></div><div id="table"></div><div id="pagination" class="pagination"></div></section>` : ''}
    <section id="preview" class="panel preview" hidden></section>
    <section class="panel results"><div class="section-top"><h2>${t('evidence')}</h2><button id="open-results" class="secondary">${t('openFolder')}</button></div><p class="hint">${t('resultsHelp')}</p><div id="results"></div></section>
    <details class="panel log"><summary>${t('logs')}</summary><pre id="logs"></pre></details>
    <footer>${t('local')}</footer></main>`;
  $('language').onchange = () => { language = ($<HTMLSelectElement>('language').value as Language); localStorage.setItem('framend-language', language); shell(); };
  $('recovery-mode').onclick = () => changeMode('recovery'); $('repair-mode').onclick = () => changeMode('repair');
  $('pick-source').onclick = () => choose('source'); $('pick-output').onclick = () => choose('output');
  if (mode === 'repair') $('pick-reference').onclick = () => choose('reference');
  else {
    $('method').onchange = () => { method = $<HTMLSelectElement>('method').value as typeof method; resetSource(); shell(); };
    $('search').oninput = () => { search = $<HTMLInputElement>('search').value; page = 0; renderCandidates(); };
    document.querySelectorAll<HTMLButtonElement>('[data-filter]').forEach(b => b.onclick = () => { filter = b.dataset.filter as Category; page = 0; shell(); });
    $('select-page').onclick = () => { visible().forEach(e => { if (e.recoverable) selected.add(e.selection); }); update(); };
    $('clear-selection').onclick = () => { selected.clear(); update(); };
    $('recover').onclick = () => start('recover');
  }
  $('pick-ffmpeg').onclick = () => choose('ffmpeg'); $('pick-ffprobe').onclick = () => choose('ffprobe');
  if (ffmpeg) $('clear-ffmpeg').onclick = () => { ffmpeg = ''; shell(); };
  if (ffprobe) $('clear-ffprobe').onclick = () => { ffprobe = ''; shell(); };
  $('start').onclick = () => start(mode === 'repair' ? 'repair' : 'scan');
  $('cancel').onclick = async () => {
    if (!busy() || cancelling) return; cancelling = true; update();
    try { await invoke('cancel_job', { jobId: state.id }); } catch (e) { log(String(e)); cancelling = false; update(); }
  };
  $('open-results').onclick = async () => { const path = state.directory || output; if (path) try { await invoke('open_result_directory', { path }); } catch (e) { log(String(e)); update(); } };
  update();
}
function changeMode(next: typeof mode) { if (busy()) return; mode = next; source = ''; reference = ''; resetSource(); shell(); }
function resetSource() { state.entries = []; state.results = []; state.directory = null; state.status = 'idle'; state.error = null; selected.clear(); preview = null; previewGeneration++; page = 0; }
async function choose(kind: 'source' | 'reference' | 'output' | 'ffmpeg' | 'ffprobe') {
  if (busy() || !ready) return;
  try {
    const path = await open({ directory: kind === 'output', multiple: false, title: t(kind === 'source' ? 'source' : kind), defaultPath: kind === 'output' ? output || undefined : undefined });
    if (!path || Array.isArray(path)) return;
    if (kind === 'source') { source = path; resetSource(); } else if (kind === 'reference') reference = path;
    else if (kind === 'output') output = path; else if (kind === 'ffmpeg') ffmpeg = path; else ffprobe = path;
    shell();
  } catch (e) { log(String(e)); update(); }
}
async function start(operation: 'scan' | 'recover' | 'repair') {
  if (busy() || !ready || !source) return;
  const tools = { ...(ffmpeg ? { ffmpeg } : {}), ...(ffprobe ? { ffprobe } : {}) };
  let request: Request;
  if (operation === 'scan') { request = { operation, source, method }; selected.clear(); preview = null; previewGeneration++; }
  else if (operation === 'recover') { const selections = selectedEntries(state.entries, selected); if (!selections.length || !output) return; request = { operation, source, output, selections, ...tools }; }
  else { if (!reference || !output) return; request = { operation, source, reference, output, ...tools }; }
  const id = crypto.randomUUID(); state.begin(id, operation); cancelling = false; log(`${t(operation === 'scan' ? 'scan' : operation === 'recover' ? 'recover' : 'repairAction')} · ${source}`); shell();
  try { await invoke('start_job', { jobId: id, request }); }
  catch (e) { state.accept({ job_id: id, payload: { event: 'terminal', success: false, cancelled: false, error: String(e) } }); log(String(e)); shell(); }
}
function filtered() { return state.entries.filter(e => (filter === 'all' || category(e.format, e.name) === filter) && e.name.toLocaleLowerCase().includes(search.toLocaleLowerCase())); }
function visible() { return filtered().slice(page * pageSize, (page + 1) * pageSize); }
function renderCandidates() {
  if (mode !== 'recovery') return;
  const entries = filtered(); const pages = Math.max(1, Math.ceil(entries.length / pageSize)); page = Math.min(page, pages - 1);
  const current = entries.slice(page * pageSize, (page + 1) * pageSize);
  $('found').textContent = `${state.entries.length.toLocaleString()} ${t('found')}`;
  $('selection-summary').textContent = `${selected.size.toLocaleString()} ${t('selected')}`;
  $('table').innerHTML = !current.length ? `<p class="empty">${t(state.entries.length ? 'emptyFilter' : 'empty')}</p>` : `<div class="table-scroll"><table><thead><tr><th class="check-column"><span class="sr-only">${t('selected')}</span></th><th>${t('name')}</th><th>${t('format')}</th><th>${t('size')}</th><th>${t('inspection')}</th><th>${t('preview')}</th></tr></thead><tbody>${current.map((e, i) => `<tr><td><input type="checkbox" data-select="${i}" aria-label="${esc(e.name)}" ${selected.has(e.selection) ? 'checked' : ''} ${!e.recoverable || busy() ? 'disabled' : ''}></td><td><span class="file-name" title="${esc(e.name)}">${esc(e.name)}</span>${e.warnings.map(w => `<small class="warning">${esc(warningText(w))}</small>`).join('')}</td><td>${esc(e.format.toUpperCase())}</td><td class="numeric">${size(e.bytes)}</td><td class="inspection">${esc(inspection(e.inspection))}${!e.recoverable ? `<small class="warning">${t('unavailable')}</small>` : ''}</td><td>${['jpeg','jpg','png','raf'].includes(e.format.toLowerCase()) ? `<button data-preview="${i}" class="text-button" ${busy() ? 'disabled' : ''}>${t('preview')}</button>` : '—'}</td></tr>`).join('')}</tbody></table></div>`;
  document.querySelectorAll<HTMLInputElement>('[data-select]').forEach(c => c.onchange = () => { const token = current[Number(c.dataset.select)].selection; c.checked ? selected.add(token) : selected.delete(token); renderButtons(); $('selection-summary').textContent = `${selected.size.toLocaleString()} ${t('selected')}`; });
  document.querySelectorAll<HTMLButtonElement>('[data-preview]').forEach(b => b.onclick = () => showPreview(current[Number(b.dataset.preview)]));
  $('pagination').innerHTML = `<span>${entries.length.toLocaleString()} ${t('count')} · ${page + 1} / ${pages}</span><div><button id="previous" class="text-button" ${page === 0 ? 'disabled' : ''}>${t('previous')}</button><button id="next" class="text-button" ${page + 1 >= pages ? 'disabled' : ''}>${t('next')}</button></div>`;
  $('previous').onclick = () => { page--; renderCandidates(); }; $('next').onclick = () => { page++; renderCandidates(); };
  ($('select-page') as HTMLButtonElement).disabled = busy() || !current.some(e => e.recoverable);
  ($('clear-selection') as HTMLButtonElement).disabled = busy() || !selected.size;
}
async function showPreview(entry: Entry) {
  const generation = ++previewGeneration; const previewSource = source;
  preview = { name: entry.name, format: entry.format }; renderPreview();
  try {
    const result = await invoke<{ mime: string; base64: string }>('preview_image', { source: previewSource, selection: entry.selection });
    if (generation !== previewGeneration || source !== previewSource) return;
    if (!['image/jpeg', 'image/png'].includes(result.mime)) throw new Error('Unsupported preview MIME');
    preview.url = `data:${result.mime};base64,${result.base64}`;
  } catch (e) { if (generation !== previewGeneration) return; preview!.error = String(e); }
  renderPreview();
}
function renderPreview() {
  const panel = $('preview'); panel.hidden = !preview; if (!preview) return;
  panel.innerHTML = `<div class="section-top"><h2>${esc(preview.name)}</h2><button id="close-preview" class="text-button">${t('closePreview')}</button></div>${preview.url ? `<img src="${preview.url}" alt="${esc(preview.name)}">` : `<p class="hint">${preview.error ? `${t('previewError')}: ${esc(preview.error)}` : t('loading')}</p>`}<p class="hint">${t(preview.format.toLowerCase() === 'raf' ? 'previewRaw' : 'previewHint')}</p>`;
  $('close-preview').onclick = () => { previewGeneration++; preview = null; renderPreview(); };
  const image = panel.querySelector('img'); if (image) image.onerror = () => { preview!.url = undefined; preview!.error = t('inspectionFailed'); renderPreview(); };
}
function renderButtons() {
  ($('start') as HTMLButtonElement).disabled = !ready || busy() || !source || (mode === 'repair' && (!reference || !output));
  if (mode === 'recovery') { ($('recover') as HTMLButtonElement).disabled = !ready || busy() || !output || !selected.size; $('selected-count').textContent = selected.size ? `(${selected.size})` : ''; }
  ($('cancel') as HTMLButtonElement).disabled = !busy() || cancelling; $('cancel').textContent = t(cancelling ? 'cancelling' : 'cancel');
  ($('open-results') as HTMLButtonElement).disabled = busy() || !(state.directory || (state.operation !== 'scan' && output && ['failed', 'cancelled'].includes(state.status)));
}
function renderProgress() {
  const p = state.progress; const elapsed = state.startedAt ? Math.floor((Date.now() - state.startedAt) / 1000) : 0;
  const percentage = p && p.total && p.total > 0 ? Math.max(0, Math.min(100, p.completed / p.total * 100)) : null;
  $('status').className = `status ${state.status}`;
  $('status').innerHTML = `<div><strong>${t(state.status)}</strong>${busy() ? `<span>${esc(phaseText(p?.phase || ''))} · ${elapsed} ${t('seconds')}${p ? ` · ${p.completed.toLocaleString()}${p.total ? ` / ${p.total.toLocaleString()} (${percentage!.toFixed(1)}%)` : ''}` : ''}</span>` : state.status === 'complete' ? `<span>${state.operation === 'scan' ? t('foundLabel') : t('successful')} ${state.count} · ${t('failures')} ${state.failed}</span>` : ''}</div>${busy() ? `<progress ${percentage === null ? '' : `value="${percentage}" max="100"`} aria-label="${t('running')}"></progress>` : ''}${state.error ? `<p role="alert">${esc(state.error)}</p>` : ''}${state.status === 'failed' || state.status === 'cancelled' ? `<p>${t('incomplete')}</p>` : ''}${!ready ? `<p>${t('nativeRequired')}</p>` : ''}`;
}
function update() {
  renderButtons(); renderProgress(); renderCandidates(); renderPreview();
  $('results').innerHTML = state.results.length ? `<ul class="result-list">${state.results.slice(-500).map(r => `<li><div><strong>${esc(r.name)}</strong><span>${size(r.bytes)} · ${esc(inspection(r.inspection))}${r.partial ? ` · ${t('partial')}` : ''}</span></div><p class="path">${esc(r.path)}</p>${r.sha256 ? `<p class="hash">${t('hash')} ${esc(r.sha256)}</p>` : ''}${r.error ? `<p class="warning">${esc(r.error)}</p>` : ''}</li>`).join('')}</ul>${state.results.length > 500 ? `<p class="hint">500 / ${state.results.length.toLocaleString()} · ${t('openFolder')}</p>` : ''}` : `<p class="empty">${t('noResults')}</p>`;
  $('logs').textContent = logs.join('\n');
}
shell();
if (isTauri()) {
  listen<Envelope>('framend-event', ({ payload }) => {
    if (!state.accept(payload)) return;
    if (payload.payload.event === 'terminal') { cancelling = false; log(`${t(state.status)}${state.error ? `: ${state.error}` : ''}`); shell(); }
    else if (!renderTimer) { renderTimer = window.setTimeout(() => { renderTimer = 0; update(); }, 100); }
  }).then(() => { ready = true; update(); }).catch(e => { log(String(e)); update(); });
}
window.setInterval(() => { if (busy()) renderProgress(); }, 1000);
