/* Apple iWork (.pages, .numbers, .key): the preview Apple's apps put in the file. Part
   iwork, on zip. The documents themselves are Apple's own (IWA: snappy-packed protobuf) and
   are not read; a saved file carries a picture of its first page (preview.jpg), an older one
   a PDF of all of it (QuickLook/Preview.pdf), which is shown instead. */
'use strict';

part('iwork', ({ ZIP }) => {

const PICS = ['preview.jpg', 'QuickLook/Thumbnail.jpg', 'preview-web.jpg', 'QuickLook/Thumbnail.png', 'preview-micro.jpg'];

async function readIwork(bytes) {
  let files;
  try { files = new Map(ZIP.read(bytes).map(f => [f.name, f])); } catch (e) { throw new Error('not an iWork file osada can open (a package folder, or not a zip)'); }
  const pdf = files.get('QuickLook/Preview.pdf');
  if (pdf) return { url: URL.createObjectURL(new Blob([await ZIP.inflate(pdf)], { type: 'application/pdf' })), pdf: true };
  for (const n of PICS) {
    const f = files.get(n);
    if (f) return { url: URL.createObjectURL(new Blob([await ZIP.inflate(f)], { type: n.endsWith('.png') ? 'image/png' : 'image/jpeg' })), pdf: false };
  }
  throw new Error('no preview in this iWork file');
}

class IworkView {
  constructor(host, r) {
    this.url = r.url;
    clear(host).append(h('div', { class: 'viewer-note dim', text: r.pdf ? tx('the preview Apple\'s app saved in the file') : tx('the preview Apple\'s app saved in the file: its first page only') }),
      r.pdf ? h('iframe', { src: r.url, title: tx('preview') }) : h('div', { class: 'iwork-box' }, h('img', { src: r.url, alt: '' })));
    host.classList.add(r.pdf ? 'iwork-pdf' : 'iwork-pic');
  }
  destroy() { URL.revokeObjectURL(this.url); }
}

return { readIwork, IworkView };
});
