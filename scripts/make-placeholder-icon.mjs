// Generates a plain 1024x1024 PNG for `tauri icon` to derive every platform size from.
// Deliberately dependency-free: it is a placeholder, and pulling an image library in to draw a
// solid square would be the wrong trade.
//
// `tauri icon assets/icon.png -o crates/zyris-app/icons` also emits Android, iOS, and Windows
// Store (Square*.png, StoreLogo.png) assets with no flag to suppress them. This product targets
// only Windows and Linux today, so prune those directories/files after regenerating.
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";

const SIZE = 1024;
const COLOR = [0x1f, 0x2a, 0x44];

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buf) {
  let c = -1;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const typed = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(typed));
  return Buffer.concat([length, typed, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 2; // colour type: truecolour

const row = Buffer.alloc(1 + SIZE * 3);
for (let x = 0; x < SIZE; x++) {
  row[1 + x * 3] = COLOR[0];
  row[2 + x * 3] = COLOR[1];
  row[3 + x * 3] = COLOR[2];
}
const raw = Buffer.concat(Array.from({ length: SIZE }, () => row));

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync("assets", { recursive: true });
writeFileSync("assets/icon.png", png);
console.log(`wrote assets/icon.png (${png.length} bytes)`);
