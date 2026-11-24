import { readdir, readFile, writeFile } from 'node:fs/promises';
import { brotliCompressSync, gzipSync } from 'node:zlib';

async function compress(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const file = new URL(entry.name, directory);
    if (entry.isDirectory()) {
      await compress(new URL(`${entry.name}/`, directory));
    } else if (
      entry.isFile() &&
      /\.(html|[cm]?js|css|svg|json|txt|xml)$/i.test(entry.name)
    ) {
      const data = await readFile(file);
      await writeFile(
        new URL(`${entry.name}.br`, directory),
        brotliCompressSync(data),
      );
      await writeFile(
        new URL(`${entry.name}.gz`, directory),
        gzipSync(data, { level: 9 }),
      );
    }
  }
}

await compress(new URL('./dist/', import.meta.url));
