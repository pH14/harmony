// SPDX-License-Identifier: AGPL-3.0-or-later
const DATABASE = "harmony-roms",
  STORE = "roms";

function open() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function transact(mode, run) {
  return open().then(
    (db) =>
      new Promise((resolve, reject) => {
        const request = run(db.transaction(STORE, mode).objectStore(STORE));
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      }).finally(() => db.close()),
  );
}

export async function load(key) {
  try {
    return (await transact("readonly", (store) => store.get(key))) ?? null;
  } catch {
    return null;
  }
}

export async function save(key, value) {
  try {
    await transact("readwrite", (store) => store.put(value, key));
    return true;
  } catch {
    return false;
  }
}

export async function loadRom(id) {
  const bytes = await load(id);
  return bytes ? new Uint8Array(bytes) : null;
}

export function saveRom(id, bytes) {
  return save(id, bytes);
}
