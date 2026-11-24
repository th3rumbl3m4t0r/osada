/* Excel 97-2003 workbooks (.xls, .xlt; BIFF8, and the values of BIFF5 from Excel 5/95),
   read into the workbook model (SheetBook) for the grid, "edit as .ods" and the xlsx
   export. Part xls, on cfb and sheet. Formulas are turned back into text from their
   tokens (Excel's names, A1 references); one that cannot be keeps its value. Macros
   (_VBA_PROJECT) are noticed, never read or run: book.macros. */
'use strict';

part('xls', ({ CFB, SheetBook, NumFmt, XErr, E, CSV, MAXC, openBook: zipBook }) => {

/* the built-in functions by number (MS-XLS Ftab); the second value is the argument count of
   those stored without one (PtgFunc) */
const FTAB = (() => {
  const t = {};
  const add = (from, names) => names.split(' ').forEach((n, i) => { if (n !== '-') t[from + i] = n; });
  add(0, 'COUNT IF ISNA ISERROR SUM AVERAGE MIN MAX ROW COLUMN NA NPV STDEV DOLLAR FIXED SIN COS TAN ATAN PI SQRT EXP LN LOG10 ABS INT ' +
    'SIGN ROUND LOOKUP INDEX REPT MID LEN VALUE TRUE FALSE AND OR NOT MOD DCOUNT DSUM DAVERAGE DMIN DMAX DSTDEV VAR DVAR TEXT LINEST ' +
    'TREND LOGEST GROWTH GOTO HALT RETURN PV FV NPER PMT RATE MIRR IRR RAND MATCH DATE TIME DAY MONTH YEAR WEEKDAY HOUR MINUTE SECOND NOW ' +
    'AREAS ROWS COLUMNS OFFSET ABSREF RELREF ARGUMENT SEARCH TRANSPOSE ERROR STEP TYPE ECHO SET.NAME CALLER DEREF WINDOWS SERIES ' +
    'DOCUMENTS ACTIVE.CELL SELECTION RESULT ATAN2 ASIN ACOS CHOOSE HLOOKUP VLOOKUP LINKS INPUT ISREF GET.FORMULA GET.NAME SET.VALUE LOG ' +
    'EXEC CHAR LOWER UPPER PROPER LEFT RIGHT EXACT TRIM REPLACE SUBSTITUTE CODE NAMES DIRECTORY FIND CELL ISERR ISTEXT ISNUMBER ISBLANK ' +
    'T N FOPEN FCLOSE FSIZE FREADLN FREAD FWRITELN FWRITE FPOS DATEVALUE TIMEVALUE SLN SYD DDB GET.DEF REFTEXT TEXTREF INDIRECT REGISTER ' +
    'CALL ADD.BAR ADD.MENU ADD.COMMAND ENABLE.COMMAND CHECK.COMMAND RENAME.COMMAND SHOW.BAR DELETE.MENU DELETE.COMMAND GET.CHART.ITEM ' +
    'DIALOG.BOX CLEAN MDETERM MINVERSE MMULT FILES IPMT PPMT COUNTA CANCEL.KEY FOR WHILE BREAK NEXT INITIATE REQUEST POKE EXECUTE ' +
    'TERMINATE RESTART HELP GET.BAR PRODUCT FACT GET.CELL GET.WORKSPACE GET.WINDOW GET.DOCUMENT DPRODUCT ISNONTEXT GET.NOTE NOTE STDEVP ' +
    'VARP DSTDEVP DVARP TRUNC ISLOGICAL DCOUNTA DELETE.BAR UNREGISTER - - USDOLLAR FINDB SEARCHB REPLACEB LEFTB RIGHTB MIDB LENB ROUNDUP ' +
    'ROUNDDOWN ASC DBCS RANK - - ADDRESS DAYS360 TODAY VDB - - - - MEDIAN SUMPRODUCT SINH COSH TANH ASINH ACOSH ATANH DGET CREATE.OBJECT ' +
    'VOLATILE LAST.ERROR CUSTOM.UNDO CUSTOM.REPEAT FORMULA.CONVERT GET.LINK.INFO TEXT.BOX INFO GROUP GET.OBJECT DB PAUSE - - RESUME ' +
    'FREQUENCY ADD.TOOLBAR DELETE.TOOLBAR - RESET.TOOLBAR EVALUATE GET.TOOLBAR GET.TOOL SPELLING.CHECK ERROR.TYPE APP.TITLE WINDOW.TITLE ' +
    'SAVE.TOOLBAR ENABLE.TOOL PRESS.TOOL REGISTER.ID GET.WORKBOOK AVEDEV BETADIST GAMMALN BETAINV BINOMDIST CHIDIST CHIINV COMBIN ' +
    'CONFIDENCE CRITBINOM EVEN EXPONDIST FDIST FINV FISHER FISHERINV FLOOR GAMMADIST GAMMAINV CEILING HYPGEOMDIST LOGNORMDIST LOGINV ' +
    'NEGBINOMDIST NORMDIST NORMSDIST NORMINV NORMSINV STANDARDIZE ODD PERMUT POISSON TDIST WEIBULL SUMXMY2 SUMX2MY2 SUMX2PY2 CHITEST ' +
    'CORREL COVAR FORECAST FTEST INTERCEPT PEARSON RSQ STEYX SLOPE TTEST PROB DEVSQ GEOMEAN HARMEAN SUMSQ KURT SKEW ZTEST LARGE SMALL ' +
    'QUARTILE PERCENTILE PERCENTRANK MODE TRIMMEAN TINV - MOVIE.COMMAND GET.MOVIE CONCATENATE POWER PIVOT.ADD.DATA GET.PIVOT.TABLE ' +
    'GET.PIVOT.FIELD GET.PIVOT.ITEM RADIANS DEGREES SUBTOTAL SUMIF COUNTIF COUNTBLANK SCENARIO.GET OPTIONS.LISTS.GET ISPMT DATEDIF ' +
    'DATESTRING NUMBERSTRING ROMAN OPEN.DIALOG SAVE.DIALOG VIEW.GET GETPIVOTDATA HYPERLINK PHONETIC AVERAGEA MAXA MINA STDEVPA VARPA ' +
    'STDEVA VARA BAHTTEXT THAIDAYOFWEEK THAIDIGIT THAIMONTHOFYEAR THAINUMSOUND THAINUMSTRING THAISTRINGLENGTH ISTHAIDIGIT ' +
    'ROUNDBAHTDOWN ROUNDBAHTUP THAIYEAR RTD');
  return t;
})();
const FARGS = {
  2: 1, 3: 1, 10: 0, 15: 1, 16: 1, 17: 1, 18: 1, 19: 0, 20: 1, 21: 1, 22: 1, 23: 1, 24: 1, 25: 1, 26: 1, 27: 2, 30: 2, 31: 3, 32: 1, 33: 1,
  34: 0, 35: 0, 38: 1, 39: 2, 40: 3, 41: 3, 42: 3, 43: 3, 44: 3, 45: 3, 47: 3, 48: 2, 61: 3, 63: 0, 65: 3, 66: 3, 67: 1, 68: 1, 69: 1,
  71: 1, 72: 1, 73: 1, 74: 0, 75: 1, 76: 1, 77: 1, 83: 1, 86: 1, 97: 2, 98: 1, 99: 1, 105: 1, 111: 1, 112: 1, 113: 1, 114: 1, 117: 2,
  118: 1, 119: 4, 121: 1, 126: 1, 127: 1, 128: 1, 129: 1, 130: 1, 131: 1, 140: 1, 141: 1, 142: 3, 143: 4, 162: 1, 163: 1, 164: 1, 165: 2,
  184: 1, 189: 3, 190: 1, 195: 3, 196: 3, 198: 1, 199: 3, 207: 4, 210: 3, 211: 1, 212: 2, 213: 2, 214: 1, 215: 1, 221: 0, 229: 1, 230: 1,
  231: 1, 232: 1, 233: 1, 234: 1, 235: 3, 244: 1, 252: 2, 261: 1, 271: 1, 273: 4, 274: 2, 275: 2, 276: 2, 277: 3, 278: 3, 279: 1, 280: 3,
  281: 3, 282: 3, 283: 1, 284: 1, 285: 2, 286: 4, 287: 3, 288: 2, 289: 4, 290: 3, 291: 3, 292: 3, 293: 4, 294: 1, 295: 3, 296: 1, 297: 3,
  298: 1, 299: 2, 300: 3, 301: 3, 302: 4, 303: 2, 304: 2, 305: 2, 306: 2, 307: 2, 308: 2, 309: 3, 310: 2, 311: 2, 312: 2, 313: 2, 314: 2,
  315: 2, 316: 4, 325: 2, 326: 2, 327: 2, 328: 2, 331: 2, 332: 2, 337: 2, 342: 1, 343: 1, 346: 2, 347: 1, 350: 4, 351: 3, 352: 1, 353: 2,
  360: 1,
};
const ERRS = { 0x00: '#NULL!', 0x07: E.div, 0x0F: E.value, 0x17: E.ref, 0x1D: E.name, 0x24: E.num, 0x2A: E.na };
const BINOP = { 0x03: '+', 0x04: '-', 0x05: '*', 0x06: '/', 0x07: '^', 0x08: '&', 0x09: '<', 0x0A: '<=', 0x0B: '=', 0x0C: '>=', 0x0D: '>', 0x0E: '<>', 0x0F: ' ', 0x10: ',', 0x11: ':' };

/* Excel's default palette (index 8 on; 0-7 repeat the first eight) */
const PALETTE = ('000000 FFFFFF FF0000 00FF00 0000FF FFFF00 FF00FF 00FFFF 800000 008000 000080 808000 800080 008080 C0C0C0 808080 ' +
  '9999FF 993366 FFFFCC CCFFFF 660066 FF8080 0066CC CCCCFF 000080 FF00FF FFFF00 00FFFF 800080 800000 008080 0000FF 00CCFF CCFFFF ' +
  'CCFFCC FFFF99 99CCFF FF99CC CC99FF FFCC99 3366FF 33CCCC 99CC00 FFCC00 FF9900 FF6600 666699 969696 003366 339966 003300 333300 ' +
  '993300 993366 333399 333333').split(' ');

/* the records of a stream: [type, data, continues] */
function records(s, from, to) {
  const dv = new DataView(s.buffer, s.byteOffset, s.byteLength), out = [];
  let p = from || 0;
  const end = Math.min(to || s.length, s.length);
  while (p + 4 <= end) {
    const type = dv.getUint16(p, true), len = dv.getUint16(p + 2, true);
    const data = s.subarray(p + 4, Math.min(p + 4 + len, s.length));
    p += 4 + len;
    if (type === 0x003C && out.length) out[out.length - 1][2].push(data);
    else out.push([type, data, []]);
  }
  return out;
}

const u16 = (d, o) => d[o] | (d[o + 1] << 8);
const i16 = (d, o) => (u16(d, o) << 16) >> 16;
const u32 = (d, o) => (d[o] | (d[o + 1] << 8) | (d[o + 2] << 16) | (d[o + 3] << 24)) >>> 0;
const f64 = (d, o) => new DataView(d.buffer, d.byteOffset + o, 8).getFloat64(0, true);
/* characters: UTF-16LE, or one byte each for the first 256 code points */
function chars(d, o, n, wide) {
  let s = '';
  for (let i = 0; i < n; i++) s += String.fromCharCode(wide ? u16(d, o + i * 2) : d[o + i]);
  return s;
}

/* a number in 30 bits: an integer, or the top of a double; maybe divided by 100 */
function rk(v) {
  let n;
  if (v & 2) n = v >> 2;
  else {
    const dv = new DataView(new ArrayBuffer(8));
    dv.setUint32(4, (v & 0xFFFFFFFC) >>> 0, true);
    n = dv.getFloat64(0, true);
  }
  return v & 1 ? n / 100 : n;
}

/* a string that may run on into CONTINUE records: there, the characters go on behind a new
   flags byte (wide or not), and the run and phonetic data after them may be split anywhere */
class Chunks {
  constructor(first, rest) { this.c = [first].concat(rest); this.i = 0; this.p = 0; }
  get d() { return this.c[this.i]; }
  room() { while (this.i < this.c.length && this.p >= this.d.length) { this.i++; this.p = 0; } return this.i < this.c.length; }
  byte() { if (!this.room()) throw new Error('xls: a string runs past its record'); return this.d[this.p++]; }
  u16() { return this.byte() | (this.byte() << 8); }
  u32() { return (this.u16() | (this.u16() << 16)) >>> 0; }
  skip(n) { while (n > 0) { if (!this.room()) return; const k = Math.min(n, this.d.length - this.p); this.p += k; n -= k; } }
  str() {
    const cch = this.u16(), fl = this.byte();
    let wide = fl & 1;
    const runs = fl & 8 ? this.u16() : 0, ext = fl & 4 ? this.u32() : 0;
    let s = '';
    while (s.length < cch) {
      if (this.p >= this.d.length) {   // on in the next CONTINUE, behind a new flags byte
        this.i++;
        this.p = 0;
        if (this.i >= this.c.length || !this.d.length) break;
        wide = this.d[this.p++] & 1;
        continue;
      }
      const n = Math.min(cch - s.length, Math.floor((this.d.length - this.p) / (wide ? 2 : 1)));
      if (n <= 0) { this.p = this.d.length; continue; }   // half a character: broken
      s += chars(this.d, this.p, n, wide);
      this.p += n * (wide ? 2 : 1);
    }
    this.skip(runs * 4 + ext);
    return s;
  }
}

/* a string with a 1- or 2-byte length (BIFF8: and its flags) at o; [text, bytes used] */
function xlStr(d, o, short, biff5, dec) {
  const cch = short ? d[o] : u16(d, o);
  let p = o + (short ? 1 : 2);
  if (biff5) return [dec(d.subarray(p, p + cch)), p + cch - o];
  const fl = d[p++];
  const runs = fl & 8 ? u16(d, p) : 0;
  if (fl & 8) p += 2;
  const ext = fl & 4 ? u32(d, p) : 0;
  if (fl & 4) p += 4;
  const s = chars(d, p, cch, fl & 1);
  p += cch * (fl & 1 ? 2 : 1) + runs * 4 + ext;
  return [s, p - o];
}

function sheetRef(name) {
  return /^[A-Za-z_À-￿][\w.À-￿]*$/.test(name) && !/^[A-Za-z]{1,3}\d+$/.test(name) ? name : "'" + name.replace(/'/g, "''") + "'";
}

class XlsBook extends SheetBook {
  static async open(bytes) {
    const b = new XlsBook();
    b.load(bytes);
    return b;
  }

  load(bytes) {
    const cf = CFB.read(bytes);
    const s = cf.get('Workbook') || cf.get('Book');
    if (!s) throw new Error('not an Excel workbook (no Workbook stream)');
    this.macros = cf.list.some(x => /^(_VBA_PROJECT_CUR|_VBA_PROJECT|Macros)$/i.test(x.name));
    const recs = records(s);
    if (!recs.length || recs[0][0] !== 0x0809) throw new Error('not an Excel workbook (no BOF)');
    const vers = u16(recs[0][1], 0);
    this.biff5 = vers === 0x0500 || vers === 0x0400;
    if (!this.biff5 && vers !== 0x0600) throw new Error('this Excel format (BIFF version ' + vers.toString(16) + ') is not read');
    let cp = 'windows-1252';
    const dec = d => { try { return new TextDecoder(cp).decode(d); } catch (e) { return chars(d, 0, d.length, false); } };
    this.d1904 = false;
    this.strings = [];
    const fmts = Object.assign({}, NumFmt.builtin), fonts = [], xfs = [], pal = PALETTE.slice(0, 8).concat(PALETTE);
    const bound = [];
    this.names = [];
    this.xti = [];
    this.sup = [];
    let active = 0;
    for (const [t, d, cont] of recs) {
      if (t === 0x000A) break;   // the globals end
      if (t === 0x002F) throw new Error('password-protected workbooks cannot be opened here');
      if (t === 0x0042 && d.length >= 2) { const n = u16(d, 0); cp = n === 1200 ? 'utf-16le' : n === 10000 ? 'macintosh' : n >= 1250 && n <= 1258 ? 'windows-' + n : n === 932 ? 'shift_jis' : n === 936 ? 'gbk' : n === 949 ? 'euc-kr' : n === 950 ? 'big5' : 'windows-1252'; }
      else if (t === 0x0022 && d.length >= 2) this.d1904 = u16(d, 0) === 1;
      else if (t === 0x003D && d.length >= 12) active = u16(d, 10);
      else if (t === 0x041E) { const [code] = xlStr(d, 2, this.biff5, this.biff5, dec); fmts[u16(d, 0)] = code; }
      else if (t === 0x0031) {
        const fl = u16(d, 2), bls = u16(d, 6);
        fonts.push({ b: bls >= 700, i: !!(fl & 2), s: !!(fl & 8), u: d[10] !== 0 });
      } else if (t === 0x0092) { const n = u16(d, 0); for (let i = 0; i < n && 2 + i * 4 + 3 <= d.length; i++) pal[8 + i] = [d[2 + i * 4], d[3 + i * 4], d[4 + i * 4]].map(x => x.toString(16).padStart(2, '0')).join('').toUpperCase(); }
      else if (t === 0x00E0) xfs.push(d);
      else if (t === 0x0085) {
        const [name] = xlStr(d, 6, true, this.biff5, dec);
        bound.push({ pos: u32(d, 0), dt: d[5], name });
      } else if (t === 0x00FC && !this.biff5) {
        const ch = new Chunks(d, cont);
        ch.skip(4);
        const n = ch.u32();
        for (let i = 0; i < n && ch.room(); i++) this.strings.push(ch.str());
      } else if (t === 0x0017 && !this.biff5) {
        for (let i = 0, n = u16(d, 0); i < n && 2 + i * 6 + 6 <= d.length; i++) this.xti.push({ sup: u16(d, 2 + i * 6), first: u16(d, 4 + i * 6), last: u16(d, 6 + i * 6) });
      } else if (t === 0x01AE) this.sup.push({ self: d.length === 4 && u16(d, 2) === 0x0401, addin: d.length === 4 && u16(d, 2) === 0x3A01, names: [] });
      else if (t === 0x0023 && this.sup.length && !this.biff5) {
        const [name] = xlStr(d, 6, true, false, dec);
        this.sup[this.sup.length - 1].names.push(name);
      } else if (t === 0x0018 && !this.biff5) this.names.push(this.nameOf(d));
    }
    const fontAt = i => fonts[i < 4 ? i : i - 1] || {};
    const color = i => (i < pal.length ? '#' + pal[i] : null);
    this.xfs = xfs.map(d => {
      const f = fontAt(u16(d, 0)), fmt = fmts[u16(d, 2)] || 'General';
      const alc = this.biff5 ? d[6] & 7 : d[6] & 7;
      const fls = this.biff5 ? (u32(d, 8) >>> 16) & 0x3F : (u32(d, 14) >>> 26) & 0x3F;
      const fore = this.biff5 ? u16(d, 8) & 0x7F : u16(d, 18) & 0x7F;
      return {
        fmt, date: NumFmt.dateKind(fmt), b: f.b, i: f.i, u: f.u, s: f.s,
        fill: fls === 1 && fore < 64 ? color(fore) : null,
        align: alc === 1 ? 'l' : alc === 2 || alc === 6 ? 'c' : alc === 3 ? 'r' : '',
      };
    });
    if (!this.xfs.length) this.xfs = [{ fmt: 'General' }];
    this.sheetNames = bound.map(x => x.name);
    this.sheets = [];
    bound.forEach((bs, i) => {
      if (bs.dt !== 0) return;   // charts, macro and module sheets
      const sh = this.readSheet(s, bs, dec);
      if (i === active) this.active = sh;
      this.sheets.push(sh);
    });
    if (!this.sheets.length) throw new Error('no worksheets in this file');
    if (!this.active) this.active = this.sheets[0];
  }

  /* a defined name (Lbl): its text; built-in ones by their meaning */
  nameOf(d) {
    const fl = u16(d, 0), cch = d[3];
    const BUILTIN = ['Consolidate_Area', 'Auto_Open', 'Auto_Close', 'Extract', 'Database', 'Criteria', 'Print_Area', 'Print_Titles', 'Recorder',
      'Data_Form', 'Auto_Activate', 'Auto_Deactivate', 'Sheet_Title', '_FilterDatabase'];
    const wide = d[14] & 1;
    const name = chars(d, 15, cch, wide);
    return fl & 0x20 ? BUILTIN[name.charCodeAt(0)] || name : name;
  }

  readSheet(s, bs, dec) {
    const sh = SheetBook.sheet(bs.name);
    sh.rowS = new Map();
    const recs = records(s, bs.pos);
    const px = w => Math.max(24, Math.round(w * 7.4 + 6));
    const shared = new Map(), arrays = new Map();
    // shared and array formulas come after their first cell: gather them first
    for (const [t, d] of recs) {
      if (t === 0x000A) break;
      if (t === 0x04BC) shared.set(u16(d, 0) + ':' + d[4], { r: u16(d, 0), c: d[4], rgce: d.subarray(10, 10 + u16(d, 8)), rgcb: d.subarray(10 + u16(d, 8)) });
      if (t === 0x0221) arrays.set(u16(d, 0) + ':' + d[4], { rows: u16(d, 2) - u16(d, 0) + 1, cols: d[5] - d[4] + 1, rgce: d.subarray(14, 14 + u16(d, 12)), rgcb: d.subarray(14 + u16(d, 12)) });
    }
    let pending = null;   // a formula whose text result is in the STRING record that follows
    const put = (r, c, s, v, f, extra) => {
      const cell = Object.assign({ s, v, f: f || null, keep: false }, extra);
      this.cell(sh, r, c, cell, f || this.rawOf(v, s));
    };
    for (const [t, d] of recs) {
      if (t === 0x000A) break;
      if (pending && t !== 0x0207) { put(...pending); pending = null; }
      switch (t) {
        case 0x00FD: put(u16(d, 0), u16(d, 2), u16(d, 4), this.strings[u32(d, 6)] === undefined ? '' : this.strings[u32(d, 6)]); break;
        case 0x0204: case 0x00D6: put(u16(d, 0), u16(d, 2), u16(d, 4), xlStr(d, 6, false, this.biff5, dec)[0]); break;
        case 0x0203: put(u16(d, 0), u16(d, 2), u16(d, 4), f64(d, 6)); break;
        case 0x027E: put(u16(d, 0), u16(d, 2), u16(d, 4), rk(u32(d, 6))); break;
        case 0x00BD: {
          const r = u16(d, 0), c0 = u16(d, 2), n = (d.length - 6) / 6;
          for (let i = 0; i < n; i++) put(r, c0 + i, u16(d, 4 + i * 6), rk(u32(d, 6 + i * 6)));
          break;
        }
        case 0x0201: put(u16(d, 0), u16(d, 2), u16(d, 4), null); break;
        case 0x00BE: { const r = u16(d, 0), c0 = u16(d, 2); for (let i = 0; 4 + i * 2 + 2 <= d.length - 2; i++) put(r, c0 + i, u16(d, 4 + i * 2), null); break; }
        case 0x0205: put(u16(d, 0), u16(d, 2), u16(d, 4), d[7] ? new XErr(ERRS[d[6]] || E.na) : !!d[6]); break;
        case 0x0006: {
          const r = u16(d, 0), c = u16(d, 2), st = u16(d, 4);
          let v;
          const textNext = u16(d, 12) === 0xFFFF && d[6] === 0;
          if (u16(d, 12) !== 0xFFFF) v = f64(d, 6);
          else v = d[6] === 1 ? !!d[8] : d[6] === 2 ? new XErr(ERRS[d[8]] || E.na) : d[6] === 3 ? '' : '';
          let f = null, extra = {};
          if (!this.biff5) {
            const cce = u16(d, 20), rgce = d.subarray(22, 22 + cce), rgcb = d.subarray(22 + cce);
            if (rgce[0] === 0x01 && cce === 5) {
              const k = u16(rgce, 1) + ':' + u16(rgce, 3);
              const a = arrays.get(k), g = shared.get(k);
              if (a && u16(rgce, 1) === r && u16(rgce, 3) === c) { f = this.formula(a.rgce, a.rgcb, r, c); extra = { keep: true, arr: { rows: a.rows, cols: a.cols } }; }
              else if (g) f = this.formula(g.rgce, g.rgcb, r, c);
            } else f = this.formula(rgce, rgcb, r, c);
          }
          if (textNext) pending = [r, c, st, '', f, extra];
          else put(r, c, st, v, f, extra);
          break;
        }
        case 0x0207: if (pending) { pending[3] = xlStr(d, 0, false, this.biff5, dec)[0]; put(...pending); pending = null; } break;
        case 0x00E5: for (let i = 0, n = u16(d, 0); i < n && 2 + i * 8 + 8 <= d.length; i++) {
          const o = 2 + i * 8, r0 = u16(d, o), r1 = u16(d, o + 2), c0 = u16(d, o + 4), c1 = u16(d, o + 6);
          if (r1 > r0 || c1 > c0) SheetBook.span(sh, r0, c0, r1 - r0 + 1, c1 - c0 + 1);
        } break;
        case 0x007D: sh.cols.push({ a: u16(d, 0), b: u16(d, 2), px: px(u16(d, 4) / 256), s: u16(d, 6) }); break;
        case 0x0055: sh.defPx = px(u16(d, 0) + 0.43); break;
        case 0x0208: if (d.length >= 16 && d[12] & 0x80) sh.rowS.set(u16(d, 0), u16(d, 14) & 0x0FFF); break;
      }
    }
    if (pending) put(...pending);
    return sh;
  }

  /* formula tokens (rgce, with rgcb for array constants) back to text, at row r, column c; null
     when something is not understood */
  formula(rgce, rgcb, r, c) {
    try { return '=' + this.decompile(rgce, rgcb, r, c); } catch (e) { return null; }
  }

  decompile(g, cb, r0, c0) {
    const st = [];
    let p = 0, q = 0;
    const pop = () => { if (!st.length) throw new Error('stack'); return st.pop(); };
    const col = (v, rel) => (rel ? '' : '$') + CSV.colName(v);
    const row = (v, rel) => (rel ? '' : '$') + (v + 1);
    const ref = (rw, cl) => col(cl & 0x3FFF, cl & 0x4000) + row(rw, cl & 0x8000);
    // relative to the formula's cell (shared formulas)
    const refN = (rw, cl) => {
      const cr = cl & 0x4000, rr = cl & 0x8000;
      const c = cr ? (c0 + ((cl & 0xFF) << 24 >> 24) + 256) % 256 : cl & 0xFF;
      const r = rr ? (r0 + ((rw << 16) >> 16) + 65536) % 65536 : rw;
      return col(c, cr) + row(r, rr);
    };
    const sheets = ixti => {
      const x = this.xti[ixti];
      if (!x) throw new Error('xti');
      const sup = this.sup[x.sup];
      if (!sup || !sup.self || x.first >= 0xFFFE) throw new Error('external');
      const a = this.sheetNames[x.first], b = this.sheetNames[x.last];
      if (a === undefined) throw new Error('sheet');
      return (x.first === x.last ? sheetRef(a) : sheetRef(a + ':' + b)) + '!';
    };
    const arrayConst = () => {
      const cols = cb[q] + 1, rows = u16(cb, q + 1) + 1;
      q += 3;
      const out = [];
      for (let i = 0; i < rows; i++) {
        const line = [];
        for (let j = 0; j < cols; j++) {
          const t = cb[q++];
          if (t === 0x01) { line.push(String(f64(cb, q))); q += 8; }
          else if (t === 0x02) { const [s, n] = xlStr(cb, q, false, false); line.push('"' + s.replace(/"/g, '""') + '"'); q += n; }
          else if (t === 0x04) { line.push(cb[q] ? 'TRUE' : 'FALSE'); q += 8; }
          else if (t === 0x10) { line.push(ERRS[cb[q]] || E.na); q += 8; }
          else { line.push(''); q += 8; }
        }
        out.push(line.join(','));
      }
      return '{' + out.join(';') + '}';
    };
    while (p < g.length) {
      const t = g[p++];
      if (BINOP[t]) { const b = pop(), a = pop(); st.push(a + BINOP[t] + b); continue; }
      switch (t) {
        case 0x12: st.push('+' + pop()); continue;
        case 0x13: st.push('-' + pop()); continue;
        case 0x14: st.push(pop() + '%'); continue;
        case 0x15: st.push('(' + pop() + ')'); continue;
        case 0x16: st.push(''); continue;
        case 0x17: { const [s, n] = xlStr(g, p, true, false); st.push('"' + s.replace(/"/g, '""') + '"'); p += n; continue; }
        case 0x19: {
          const fl = g[p];
          if (fl & 0x04) { p += 3 + 2 * (u16(g, p + 1) + 1); continue; }   // CHOOSE's jump table
          p += 3;
          if (fl & 0x10) st.push('SUM(' + pop() + ')');
          continue;
        }
        case 0x1C: st.push(ERRS[g[p++]] || E.na); continue;
        case 0x1D: st.push(g[p++] ? 'TRUE' : 'FALSE'); continue;
        case 0x1E: st.push(String(u16(g, p))); p += 2; continue;
        case 0x1F: st.push(String(f64(g, p))); p += 8; continue;
      }
      const base = t >= 0x20 && t < 0x80 ? (t & 0x1F) | 0x20 : t;   // the class (reference, value, array) aside
      switch (base) {
        case 0x20: p += 7; st.push(arrayConst()); continue;
        case 0x21: {
          const id = u16(g, p);
          p += 2;
          const n = FARGS[id], name = FTAB[id];
          if (n === undefined || !name) throw new Error('fn ' + id);
          const args = st.splice(st.length - n, n);
          if (args.length !== n) throw new Error('stack');
          st.push(name + '(' + args.join(',') + ')');
          continue;
        }
        case 0x22: {
          const n = g[p], id = u16(g, p + 1) & 0x7FFF;
          p += 3;
          const args = st.splice(st.length - n, n);
          if (args.length !== n) throw new Error('stack');
          if (id === 255) { const fn = args.shift(); st.push(fn + '(' + args.join(',') + ')'); continue; }
          if (!FTAB[id]) throw new Error('fn ' + id);
          st.push(FTAB[id] + '(' + args.join(',') + ')');
          continue;
        }
        case 0x23: { const n = this.names[u16(g, p) - 1]; p += 4; if (n === undefined) throw new Error('name'); st.push(n); continue; }
        case 0x24: st.push(ref(u16(g, p), u16(g, p + 2))); p += 4; continue;
        case 0x25: st.push(ref(u16(g, p), u16(g, p + 4)) + ':' + ref(u16(g, p + 2), u16(g, p + 6))); p += 8; continue;
        case 0x26: p += 6; q += 2 + 8 * u16(cb, q); continue;   // a sub-expression's area: its tokens follow, its cells are in rgcb
        case 0x27: case 0x28: p += 6; continue;
        case 0x29: p += 2; continue;
        case 0x2A: st.push(E.ref); p += 4; continue;
        case 0x2B: st.push(E.ref); p += 8; continue;
        case 0x2C: st.push(refN(u16(g, p), u16(g, p + 2))); p += 4; continue;
        case 0x2D: st.push(refN(u16(g, p), u16(g, p + 4)) + ':' + refN(u16(g, p + 2), u16(g, p + 6))); p += 8; continue;
        case 0x39: {
          const x = this.xti[u16(g, p)], ilbl = u16(g, p + 2);
          p += 6;
          const sup = x && this.sup[x.sup];
          const name = sup ? (sup.self ? this.names[ilbl - 1] : sup.names[ilbl - 1]) : undefined;
          if (name === undefined) throw new Error('namex');
          st.push(name);
          continue;
        }
        case 0x3A: st.push(sheets(u16(g, p)) + ref(u16(g, p + 2), u16(g, p + 4))); p += 6; continue;
        case 0x3B: st.push(sheets(u16(g, p)) + ref(u16(g, p + 2), u16(g, p + 6)) + ':' + ref(u16(g, p + 4), u16(g, p + 8))); p += 10; continue;
        case 0x3C: st.push(E.ref); p += 6; continue;
        case 0x3D: st.push(E.ref); p += 10; continue;
      }
      throw new Error('token ' + t.toString(16));
    }
    if (st.length !== 1) throw new Error('stack');
    return st[0];
  }

  styleAt(sh, r, c) {
    const k = r * MAXC + c;
    if (sh.pend.has(k)) return sh.pend.get(k);
    const cell = sh.cells.get(k);
    if (cell) return cell.s;
    if (sh.rowS.has(r)) return sh.rowS.get(r);
    const cl = sh.colAt(c);
    return cl ? cl.s : 0;
  }
}

/* the book of any spreadsheet the sheet part reads, xls too */
async function openBook(bytes) {
  return CFB.is(bytes) ? XlsBook.open(bytes) : zipBook(bytes);
}

return { XlsBook, openBook, FTAB };
});
