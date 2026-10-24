// Chat rendering stack: markdown, code, sandboxed HTML, mermaid, artifacts.
import { createContext, memo, useContext, useEffect, useMemo, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import rehypeHighlight from 'rehype-highlight';
import DOMPurify from 'dompurify';
import { api } from '../api';
import 'katex/dist/katex.min.css';

/** Lets markdown blocks open the artifact viewer from anywhere in the tree. */
export const ViewerContext = createContext<(a: ViewerArtifact) => void>(() => {});

/** A single code block: highlighted source with copy/wrap/download. */
export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const [copied, setCopied] = useState(false);
  const [wrap, setWrap] = useState(false);
  return (
    <div className="codeblock">
      <div className="codeblock-bar">
        <span className="tstamp">{lang || 'code'}</span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
          <button className="tbtn" onClick={() => setWrap(!wrap)}>{wrap ? 'No wrap' : 'Wrap'}</button>
          <button className="tbtn" onClick={downloadTxt(code, lang)} title="Download">↓</button>
          <button className="tbtn" onClick={() => { navigator.clipboard.writeText(code); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>{copied ? 'Copied' : 'Copy'}</button>
        </span>
      </div>
      <pre className={wrap ? 'wrap' : ''}><code className={`hljs${wrap ? ' wrap' : ''}`}>{code}</code></pre>
    </div>
  );
}

function downloadTxt(content: string, lang?: string) {
  const ext = lang && /^[a-z0-9]{1,5}$/i.test(lang) ? `.${lang}` : '.txt';
  return () => {
    const blob = new Blob([content], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `snippet${ext}`;
    a.click();
    URL.revokeObjectURL(a.href);
  };
}

/** Fenced HTML: rendered in a sandboxed iframe (never injected into the DOM). */
export function HtmlPreview({ code }: { code: string }) {
  const [mode, setMode] = useState<'preview' | 'source'>('preview');
  const [full, setFull] = useState(false);
  const safe = useMemo(() => DOMPurify.sanitize(code), [code]);
  return (
    <div className="codeblock">
      <div className="codeblock-bar">
        <span className="tstamp">html</span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
          <button className="tbtn" onClick={() => setFull(!full)}>{full ? 'Shrink' : 'Expand'}</button>
          <button className="tbtn" onClick={() => setMode(mode === 'preview' ? 'source' : 'preview')}>{mode === 'preview' ? 'Source' : 'Preview'}</button>
        </span>
      </div>
      {mode === 'preview'
        ? <iframe
            title="HTML preview"
            sandbox=""
            srcDoc={safe}
            style={{ width: '100%', border: 0, background: '#fff', borderRadius: 8, height: full ? '70vh' : 300 }}
          />
        : <pre><code className="hljs">{code}</code></pre>}
    </div>
  );
}

/** Fenced mermaid: rendered to SVG client-side; falls back to the source. */
export function MermaidBlock({ code }: { code: string }) {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const id = useRef(`mmd-${Math.random().toString(36).slice(2, 9)}`);
  useEffect(() => {
    let alive = true;
    // mermaid is large — load it only when a diagram actually appears.
    import('mermaid').then(({ default: mermaid }) => {
      mermaid.initialize({ startOnLoad: false, theme: 'neutral' });
      mermaid.render(id.current, code).then(({ svg }) => { if (alive) setSvg(svg); }).catch(() => { if (alive) setError(true); });
    }).catch(() => { if (alive) setError(true); });
    return () => { alive = false; };
  }, [code]);
  if (error) return <CodeBlock code={code} lang="mermaid" />;
  return svg
    ? <div className="mermaid-box" dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(svg, { USE_PROFILES: { svg: true, svgFilters: true } }) }} />
    : <p className="tiny dim">Rendering diagram…</p>;
}

/** Big code blocks become artifact cards; open in the viewer pane. */
export function ArtifactCard({ lang, code, onOpen }: { lang?: string; code: string; onOpen: (a: { kind: 'code' | 'html' | 'mermaid'; lang?: string; code: string; name: string }) => void }) {
  const lines = code.split('\n').length;
  const kb = (new TextEncoder().encode(code).length / 1024).toFixed(1);
  const kind = lang === 'html' ? 'html' : lang === 'mermaid' ? 'mermaid' : 'code';
  const name = `snippet.${lang && /^[a-z0-9]{1,5}$/i.test(lang) ? lang : 'txt'}`;
  return (
    <button className="artifact-card" onClick={() => onOpen({ kind, lang, code, name })}>
      <span className="chip c-idle">{kind}</span>
      <span style={{ flex: 1, textAlign: 'left', minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{name}</span>
      <span className="tstamp">{lines} lines · {kb} KB</span>
      <span className="tiny dim">Preview →</span>
    </button>
  );
}

export interface ViewerArtifact { kind: 'code' | 'html' | 'mermaid' | 'md' | 'image' | 'text'; lang?: string; code: string; name: string; mime?: string }

/** Full viewer: rendered markdown, sandboxed HTML, images or highlighted code. */
export function ArtifactViewerPane({ artifact, onClose }: { artifact: ViewerArtifact; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const a = artifact;
  return (
    <div className="tw" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="tw-box" role="dialog" aria-modal="true" style={{ width: 'min(960px, 94vw)', maxHeight: '88vh', display: 'flex', flexDirection: 'column' }}>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 10 }}>
          <span className="chip c-idle">{a.kind}</span>
          <strong style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.name}</strong>
          {a.kind !== 'image' && <button className="tbtn" onClick={() => { navigator.clipboard.writeText(a.code); setCopied(true); setTimeout(() => setCopied(false), 1200); }}>{copied ? 'Copied' : 'Copy'}</button>}
          {a.kind !== 'image' && <button className="tbtn" onClick={downloadTxt(a.code, a.lang ?? (a.kind === 'md' ? 'md' : undefined))}>↓</button>}
          <button className="btn btn-sm" onClick={onClose}>Close</button>
        </div>
        <div style={{ overflow: 'auto', minHeight: 0, flex: 1 }}>
          {a.kind === 'html' && <HtmlPreview code={a.code} />}
          {a.kind === 'mermaid' && <MermaidBlock code={a.code} />}
          {a.kind === 'md' && <div className="md-body"><MarkdownView text={a.code} /></div>}
          {a.kind === 'image' && (
            a.mime?.startsWith('image/')
              ? <img src={`data:${a.mime};base64,${a.code}`} alt={a.name} style={{ maxWidth: '100%', borderRadius: 8 }} />
              : <img src={`data:image/svg+xml;utf8,${encodeURIComponent(a.code)}`} alt={a.name} style={{ maxWidth: '100%', borderRadius: 8 }} />
          )}
          {a.kind === 'text' && <pre className="term" style={{ maxHeight: '70vh' }}>{a.code}</pre>}
          {a.kind === 'code' && <CodeBlock code={a.code} lang={a.lang} />}
        </div>
      </div>
    </div>
  );
}

const mdComponents = {
  code({ node, className, children, ...props }: any) {
    void node;
    const raw = String(children ?? '');
    const lang = /language-(\w+)/.exec(className || '')?.[1];
    const isBlock = className?.includes('language-') || raw.includes('\n');
    if (!isBlock) return <code className={className} {...props}>{children}</code>;
    const body = raw.replace(/\n$/, '');
    const lines = body.split('\n').length;
    const openViewer = useContext(ViewerContext);
    if (lang === 'html' || (lang === undefined && /^\s*<(!doctype|html|div|body|section|main)/i.test(body))) {
      // Small html fences preview inline; large ones become artifact cards.
      if (lines > 24 || body.length > 1800) return <ArtifactCard lang="html" code={body} onOpen={openViewer} />;
      return <HtmlPreview code={body} />;
    }
    if (lang === 'mermaid') return <MermaidBlock code={body} />;
    if (lines > 24 || body.length > 1800) return <ArtifactCard lang={lang} code={body} onOpen={openViewer} />;
    return <CodeBlock code={body} lang={lang} />;
  },
  a({ href, children }: any) {
    return <a href={href} target="_blank" rel="noreferrer">{children}</a>;
  },
};

/** Markdown with GFM tables, KaTeX math, highlighted code. */
export const MarkdownView = memo(function MarkdownView({ text }: { text: string }) {
  return (
    <div className="md-body">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[rehypeKatex, [rehypeHighlight, { detect: true, ignoreMissing: true }]]}
        components={mdComponents as any}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});

/** File preview for the Files panel / viewer: md renders, html sandboxes, images inline. */
export function FilePreview({ runId, path }: { runId: string; path: string }) {
  const [state, setState] = useState<{ kind: ViewerArtifact['kind']; text?: string; mime?: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    setState(null); setError(null);
    const ext = path.split('.').pop()?.toLowerCase() ?? '';
    const imageExts = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'ico'];
    if (imageExts.includes(ext)) {
      api.runFileRaw(runId, path).then((r) => { if (alive) setState({ kind: 'image', text: r.base64, mime: r.mime }); }).catch((e) => { if (alive) setError((e as Error).message); });
      return () => { alive = false; };
    }
    api.runFile(runId, path).then((text) => {
      if (!alive) return;
      if (ext === 'md' || ext === 'markdown') setState({ kind: 'md', text });
      else if (ext === 'html' || ext === 'htm') setState({ kind: 'html', text });
      else setState({ kind: 'text', text });
    }).catch((e) => { if (alive) setError((e as Error).message); });
    return () => { alive = false; };
  }, [runId, path]);
  if (error) return <p className="tiny" style={{ color: 'var(--fail)' }}>{error}</p>;
  if (!state) return <p className="tiny dim">Loading {path}…</p>;
  if (state.kind === 'image') return <img src={state.mime?.startsWith('image/') ? `data:${state.mime};base64,${state.text}` : `data:image/svg+xml;utf8,${encodeURIComponent(state.text ?? '')}`} alt={path} style={{ maxWidth: '100%', borderRadius: 8 }} />;
  if (state.kind === 'md') return <div className="md-body"><MarkdownView text={state.text ?? ''} /></div>;
  if (state.kind === 'html') return <iframe title={path} sandbox="" srcDoc={DOMPurify.sanitize(state.text ?? '')} style={{ width: '100%', height: '60vh', border: 0, background: '#fff', borderRadius: 8 }} />;
  return <pre className="term" style={{ maxHeight: '60vh' }}>{state.text}</pre>;
}
