/* osada's office page, in LibreOffice's UNO thread (Round 18): opens the file it is told, saves
   it when told (LibreOffice's own save does the same), and tells the page when it was saved and
   when it changed. Derived from zetajs's examples/web-office (MIT, allotropia). */
'use strict';

let zetajs, css, desktop, xModel;

function post(m) { zetajs.mainPort.postMessage(m); }

function start() {
  desktop = css.frame.Desktop.create(zetajs.getUnoComponentContext());
  zetajs.mainPort.onmessage = e => {
    const d = e.data;
    try {
      if (d.cmd === 'open') open(d.url, d.ro);
      else if (d.cmd === 'save') xModel.store();
      else throw new Error('unknown command ' + d.cmd);
    } catch (err) {
      let message = String((err && err.message) || err);
      try { const x = zetajs.catchUnoException(err); if (x && x.Message) message = x.Message; } catch (e2) { /* not UNO's */ }
      post({ cmd: 'error', what: d.cmd, message });
    }
  };
  post({ cmd: 'thr_running' });
}

function open(url, ro) {
  const args = ro ? [new css.beans.PropertyValue({ Name: 'ReadOnly', Value: true })] : [];
  xModel = desktop.loadComponentFromURL(url, '_default', 0, args);
  if (!xModel) throw new Error('it is no document LibreOffice knows');
  const listener = zetajs.unoObject([css.document.XDocumentEventListener], {
    documentEventOccured(ev) {
      const n = ev.EventName;
      if (n === 'OnSaveDone' || n === 'OnSaveAsDone') post({ cmd: 'stored', url: xModel.getURL() });
      else if (n === 'OnSaveFailed' || n === 'OnSaveAsFailed') post({ cmd: 'error', what: 'save', message: 'LibreOffice could not save it' });
      else if (n === 'OnModifyChanged') post({ cmd: 'modified', on: !!xModel.isModified() });
    },
    disposing() {},
  });
  xModel.addDocumentEventListener(listener);
  xModel.getCurrentController().getFrame().getContainerWindow().FullScreen = true;
  post({ cmd: 'ui_ready' });
}

Module.zetajs.then(z => {
  zetajs = z;
  css = z.uno.com.sun.star;
  start();
});
