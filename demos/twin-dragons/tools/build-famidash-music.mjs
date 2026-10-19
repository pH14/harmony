// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const SIDES = [
  ["main", "Main game"],
  ["b-sides", "B-Sides"],
  ["c-sides", "C-Sides"],
  ["d-sides", "D-Sides"],
];
const base = new URL("public/", pathToFileURL(process.cwd() + "/"));

const json5 = (text) =>
  JSON.parse(
    text
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/([{,]\s*)([A-Za-z_]\w*)\s*:/g, '$1"$2":')
      .replace(/,(\s*[}\]])/g, "$1"),
  );

const music = [];
for (const [file, name] of SIDES) {
  const metadata = json5(await readFile(new URL(`licenses/famidash-music-${file}.json5`, base), "utf8"));
  const songs = metadata.songs.map((song) => ({
    title: song.nsfData?.trackLabel ?? song.fmsSongName,
    original: song.nsfData?.trackAuthor?.original ?? song.originalArtistText ?? "",
    cover: song.nsfData?.trackAuthor?.cover ?? "",
  }));
  if (!songs.length || songs.some((song) => !song.title)) throw new Error(`The ${name} music metadata has a song without a title`);
  music.push({ name, songs });
}
await mkdir(new URL("famidash/", base), { recursive: true });
await writeFile(new URL("famidash/music.json", base), JSON.stringify(music));
console.log("Famidash music credits", music.map(({ name, songs }) => `${name} ${songs.length}`).join(", "));
