// swai load test of the osada test cluster (test.example.net). One kind of request at a fixed
// arrival rate (requests a second, whatever the answers take: no coordinated omission).
//   MODE  me        GET /api/me, signed in: edge → link → home, a session check, JSON
//         page      GET /, the portal's page (home renders it)
//         redirect  GET http://…/ (port 80): the edge answers itself (301), nothing relayed
//         churn     as me, a new TCP+TLS connection for every request
//         signin    POST /api/signin (argon2 at home) every time
//         asset     GET a static file of the page (/static2/<hash>/app.js)
//   RATE, DUR, EDGE (an edge's address: test.example.net pinned to it), OUT (summary JSON)
import http from 'k6/http';
import { check } from 'k6';

const BASE = __ENV.BASE || 'https://test.example.net';
const MODE = __ENV.MODE || 'me';
const RATE = +(__ENV.RATE || 10);
const H = { 'content-type': 'application/json', 'x-shelf': '1' };

export const options = {
  hosts: __ENV.EDGE ? { 'test.example.net': __ENV.EDGE } : {},
  noConnectionReuse: MODE === 'churn',
  scenarios: {
    main: {
      executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s', duration: __ENV.DUR || '30s',
      preAllocatedVUs: Math.max(20, RATE), maxVUs: Math.max(100, RATE * 4), gracefulStop: '10s',
    },
  },
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  const out = {};
  if (MODE === 'me' || MODE === 'churn') {
    const r = http.post(`${BASE}/api/signin`, JSON.stringify({ name: __ENV.USER_NAME, password: __ENV.USER_PW }), { headers: H });
    if (r.status !== 200 || !r.cookies.osada) throw new Error(`sign-in: ${r.status} ${r.body}`);
    out.cookie = r.cookies.osada[0].value;
  }
  if (MODE === 'asset') {
    const p = http.get(`${BASE}/`).body || '';
    const m = p.match(/\/static2\/[0-9a-f]+\/app\.js/);
    if (!m) throw new Error('no asset in the page');
    out.asset = m[0];
  }
  return out;
}

export default function (d) {
  let r;
  if (MODE === 'me' || MODE === 'churn') {
    r = http.get(`${BASE}/api/me`, { headers: { cookie: `osada=${d.cookie}`, 'x-shelf': '1' } });
    check(r, { '200': x => x.status === 200 });
  } else if (MODE === 'page') {
    r = http.get(`${BASE}/`);
    check(r, { '200': x => x.status === 200 });
  } else if (MODE === 'asset') {
    r = http.get(`${BASE}${d.asset}`);
    check(r, { '200': x => x.status === 200 });
  } else if (MODE === 'redirect') {
    r = http.get('http://test.example.net/', { redirects: 0 });
    check(r, { '301': x => x.status === 301 });
  } else if (MODE === 'signin') {
    r = http.post(`${BASE}/api/signin`, JSON.stringify({ name: __ENV.USER_NAME, password: __ENV.USER_PW }), { headers: H });
    check(r, { '200': x => x.status === 200 });
  }
}

export function handleSummary(data) {
  return { [__ENV.OUT || '/root/load/last.json']: JSON.stringify(data.metrics) };
}
