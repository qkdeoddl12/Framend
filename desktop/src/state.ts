export interface Entry {
  id: string; name: string; format: string; bytes: number; recoverable: boolean;
  inspection: unknown; warnings: string[]; selection: string;
}
export interface Result {
  name: string; path: string; bytes: number; sha256: string | null;
  inspection: unknown; partial: boolean; error: string | null;
}
export type Payload =
  | { event: 'started' }
  | { event: 'progress'; phase: string; completed: number; total: number | null }
  | { event: 'candidate'; entry: Entry }
  | { event: 'saved'; result: Result }
  | { event: 'complete'; directory: string | null; count: number; failed: number }
  | { event: 'terminal'; success: boolean; cancelled: boolean; error: string | null };
export interface Envelope { job_id: string; payload: Payload }
export type Operation = 'scan' | 'recover' | 'repair';
export type Category = 'all' | 'image' | 'video' | 'audio' | 'document' | 'archive' | 'other';
export type Request =
  | { operation: 'scan'; source: string; method: 'carve' | 'filesystem' }
  | { operation: 'recover'; source: string; output: string; selections: string[]; ffmpeg?: string; ffprobe?: string }
  | { operation: 'repair'; source: string; reference: string; output: string; ffmpeg?: string; ffprobe?: string };

export class JobState {
  id = ''; operation: Operation = 'scan'; status: 'idle' | 'running' | 'complete' | 'failed' | 'cancelled' = 'idle';
  entries: Entry[] = []; results: Result[] = []; directory: string | null = null;
  error: string | null = null; progress: Extract<Payload, {event: 'progress'}> | null = null;
  count = 0; failed = 0; startedAt = 0;
  begin(id: string, operation: Operation) {
    this.id = id; this.operation = operation; this.status = 'running'; this.error = null;
    this.progress = null; this.directory = null; this.results = []; this.count = 0; this.failed = 0; this.startedAt = Date.now();
    if (operation === 'scan') this.entries = [];
  }
  accept(envelope: Envelope): boolean {
    if (envelope.job_id !== this.id || this.status !== 'running') return false;
    const p = envelope.payload;
    switch (p.event) {
      case 'candidate': this.entries.push(p.entry); break;
      case 'saved': this.results.push(p.result); break;
      case 'progress': this.progress = p; break;
      case 'complete': this.directory = p.directory; this.count = p.count; this.failed = p.failed; break;
      case 'terminal': this.status = p.cancelled ? 'cancelled' : p.success ? 'complete' : 'failed'; this.error = p.error; break;
    }
    return true;
  }
}
export function selectedEntries(entries: Entry[], selected: Set<string>): string[] {
  return entries.filter(e => e.recoverable && selected.has(e.selection)).map(e => e.selection);
}
export function category(format: string, name: string): Category {
  const ext = (format === 'unknown' || format === 'file' ? name.split('.').pop() || '' : format).toLowerCase();
  if (['jpg','jpeg','png','raf','raw','dng','cr2','cr3','nef','arw','tif','tiff','heic','webp','gif','bmp'].includes(ext)) return 'image';
  if (['mov','mp4','mkv','avi','m4v','mts','m2ts','webm'].includes(ext)) return 'video';
  if (['wav','wave','mp3','flac','aac','m4a','ogg','aiff'].includes(ext)) return 'audio';
  if (['pdf','doc','docx','xls','xlsx','ppt','pptx','txt','md','csv','rtf','odt'].includes(ext)) return 'document';
  if (['zip','7z','rar','gz','tar','bz2','xz'].includes(ext)) return 'archive';
  return 'other';
}
