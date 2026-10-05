// Runs each export of the wasm probe once, in this order, and says what
// came back: a code, or the trap that ended the call.
import { readFile } from 'node:fs/promises';

const bytes = await readFile('target/wasm32-unknown-unknown/release/wasm_probe.wasm');
const { instance } = await WebAssembly.instantiate(bytes, {});
for (const name of ['stage_probe', 'undo_probe', 'stage_probe']) {
  try {
    console.log(`${name}: returned ${instance.exports[name]()}`);
  } catch (e) {
    console.log(`${name}: trapped: ${e.constructor.name}: ${e.message}`);
  }
}
