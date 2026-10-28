// list_voices: the ElevenLabs voices this key can use, for voice_line.
export default async function (args, kx) {
  const key = kx.secret("ELEVENLABS_API_KEY");
  if (!key) return kx.fail("No ElevenLabs key: add it in Settings > Tools > Packs.");
  const api = kx.setting("elevenlabs_url") || "https://api.elevenlabs.io/v1";
  const res = kx.fetch(`${api}/voices`, { headers: { "xi-api-key": key } });
  if (!res.ok) return kx.fail(`ElevenLabs said ${res.status}: ${res.text()}`);
  const voices = res.json().voices || [];
  const lines = voices.map((v) => {
    const labels = Object.values(v.labels || {}).join(", ");
    return `${v.voice_id}  ${v.name}${labels ? ` (${labels})` : ""}`;
  });
  return { text: lines.length ? lines.join("\n") : "No voices on this account." };
}
