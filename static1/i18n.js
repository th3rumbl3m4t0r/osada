/* osada in EN, CZ, SK, HU, FR and AT (Austrian German, code de). The English text is the key: tx('delete') is the word in the
   chosen language, or the English when a text has none. {name} in a text is filled from vars,
   txn() picks the plural form, txErr() also knows the server's messages with a varying part.
   The texts of one language come from /static/lang-<code>.js (made by the server from
   texts.js), loaded for the language in use only; English needs none. LANG_READY settles
   when they are there. Loaded in <head> before x11.js, which asks tx() for its labels too
   and is relabelled when they arrive. The picker goes into any [data-osada-lang]; a page with
   window.onLangPick handles the change, any other reloads. */
'use strict';

/* every file under /static/ by its content hash (<meta name="osada-assets">): a URL with it
   is cached for good, and changes when the file does. Read here, the first script. */
const ASSET_V = {};
for (const kv of ((document.querySelector('meta[name="osada-assets"]') || {}).content || '').split(' ')) {
  const i = kv.indexOf('=');
  if (i > 0) ASSET_V[kv.slice(0, i)] = kv.slice(i + 1);
}
function assetUrl(file) { return '/static/' + file + '?v=' + (ASSET_V[file] || ''); }

const LANGS = [['en', 'EN'], ['cs', 'CZ'], ['sk', 'SK'], ['hu', 'HU'], ['fr', 'FR'], ['de', 'AT']];
/* the locale of the language in use, for dates and the page: German as in Austria */
function uiLocale() { return LANG === 'de' ? 'de-AT' : LANG; }
const LANG_KEY = 'osada.lang';

/* server messages with a part that varies: [pattern, key, names of the parts] */
const TEXT_RES = [
  [/^wrong username or password, or this account cannot be used from (.+)$/, 'wrong username or password, or this account cannot be used from {ip}', ['ip']],
  [/^(.+) cannot be used from (.+)$/, '{name} cannot be used from {ip}', ['name', 'ip']],
  [/^changed by (.+) since it was opened$/, 'changed by {name} since it was opened', ['name']],
];

/* this browser's last pick, else the first of its languages osada has, else English */
function firstLang() {
  const codes = LANGS.map(l => l[0]);
  let s = null;
  try { s = localStorage.getItem(LANG_KEY); } catch (e) { /* storage off */ }
  if (codes.includes(s)) return s;
  for (const l of navigator.languages || [navigator.language || '']) {
    const c = String(l).slice(0, 2).toLowerCase();
    if (codes.includes(c)) return c;
  }
  return 'en';
}
let LANG = 'en';
/* the texts of LANG: key -> text, or the plural forms */
let TEXT = {};
/* packs as lang-<code>.js hands them over (osadaTexts); only the one in use is kept */
const LANG_PACKS = {};
function osadaTexts(code, texts) { LANG_PACKS[code] = texts; }

function loadLang(code) {
  if (code === 'en' || LANG_PACKS[code]) return Promise.resolve();
  return new Promise(ok => {
    const s = document.createElement('script');
    s.src = assetUrl('lang-' + code + '.js');
    // without it the page is in English, which beats no page
    s.onload = s.onerror = () => { s.remove(); ok(); };
    document.head.appendChild(s);
  });
}

/* switches to `code` once its texts are there; `pick`: the user chose it, remembered here */
async function setLang(code, pick) {
  if (!LANGS.some(l => l[0] === code)) return;
  await loadLang(code);
  for (const c in LANG_PACKS) if (c !== code) delete LANG_PACKS[c];
  LANG = code;
  TEXT = LANG_PACKS[code] || {};
  if (pick !== false) {
    try { localStorage.setItem(LANG_KEY, code); } catch (e) { /* storage off */ }
  }
  document.documentElement.lang = uiLocale();
  document.querySelectorAll('select.lang-pick').forEach(s => { s.value = code; s.title = tx('language'); });
  if (window.X11 && window.X11.relabel) window.X11.relabel();
}
const LANG_READY = setLang(firstLang(), false);

function txFill(s, vars) {
  if (!vars || typeof vars !== 'object') return s;
  return s.replace(/\{(\w+)\}/g, (m, k) => (vars[k] === undefined || vars[k] === null ? m : String(vars[k])));
}
function txPick(key) { return TEXT[key]; }
function tx(s, vars) {
  const v = txPick(s);
  return txFill(typeof v === 'string' ? v : s, vars);
}
/* {n} items: one is the English singular, other the plural and the key */
function txn(n, one, other, vars) {
  vars = Object.assign({ n }, vars);
  const v = txPick(other);
  if (typeof v === 'string') return txFill(v, vars);
  if (Array.isArray(v)) {
    const f = LANG === 'fr' ? (n === 0 || n === 1 ? 0 : 1) : LANG === 'de' ? (n === 1 ? 0 : 1) : n === 1 ? 0 : n >= 2 && n <= 4 ? 1 : 2;
    return txFill(v[Math.min(f, v.length - 1)], vars);
  }
  return txFill(n === 1 ? one : other, vars);
}
/* an error message: the server's and the file readers' are English */
function txErr(msg) {
  msg = String(msg);
  if (LANG === 'en' || txPick(msg) !== undefined) return tx(msg);
  for (const [re, key, names] of TEXT_RES) {
    const m = re.exec(msg);
    if (m) return tx(key, Object.fromEntries(names.map((k, i) => [k, m[i + 1]])));
  }
  return msg;
}

function mountLangPick() {
  document.querySelectorAll('[data-osada-lang]').forEach(el => {
    if (el.firstChild) return;
    const s = document.createElement('select');
    s.className = 'lang-pick';
    s.title = tx('language');
    for (const [code, label] of LANGS) {
      const o = document.createElement('option');
      o.value = code;
      o.textContent = label;
      s.appendChild(o);
    }
    s.value = LANG;
    s.addEventListener('change', () => {
      if (window.onLangPick) window.onLangPick(s.value);
      else setLang(s.value).then(() => location.reload());
    });
    el.appendChild(s);
  });
}

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mountLangPick);
else mountLangPick();
