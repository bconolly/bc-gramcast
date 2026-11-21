import { readFileSync } from 'node:fs';
const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[2]));
const t0 = performance.now(); instance.exports.sign_n(1); const first = performance.now() - t0;
const n = 1000; const t1 = performance.now(); instance.exports.sign_n(n); const per = (performance.now() - t1) / n;
console.log(`first call ${first.toFixed(3)} ms, then ${per.toFixed(4)} ms per presign (${n} runs)`);
