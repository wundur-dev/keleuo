// sound_effect, voice_line and music: ElevenLabs.
export default async function (args, kx) {
  const key = kx.secret("ELEVENLABS_API_KEY");
  if (!key) return kx.fail("No ElevenLabs key: add it in Settings > Tools > Packs.");
  const api = kx.setting("elevenlabs_url") || "https://api.elevenlabs.io/v1";
  const headers = { "xi-api-key": key };
  const format = "mp3_44100_128";
  let url, body, what, about;
  if (kx.tool === "voice_line") {
    const voice = args.voice || kx.setting("voice_id");
    if (!voice) return kx.fail("No voice: give one (list_voices shows them) or set a default voice in Settings > Tools > Packs.");
    url = `${api}/text-to-speech/${encodeURIComponent(voice)}?output_format=${format}`;
    body = { text: args.text, model_id: kx.setting("voice_model") || "eleven_v3" };
    what = args.name || args.text;
    about = "voice line";
  } else if (kx.tool === "music") {
    const seconds = Math.min(Math.max(Number(args.seconds) || 30, 3), 600);
    url = `${api}/music?output_format=${format}`;
    body = { prompt: args.prompt, music_length_ms: Math.round(seconds * 1000), model_id: kx.setting("music_model") || "music_v2_5" };
    if (args.instrumental) body.force_instrumental = true;
    what = args.name || args.prompt;
    about = `${seconds}s of music`;
  } else {
    url = `${api}/sound-generation?output_format=${format}`;
    body = { text: args.prompt, model_id: kx.setting("sound_model") || "eleven_text_to_sound_v2", prompt_influence: 0.3 };
    if (args.seconds) body.duration_seconds = Math.min(Math.max(Number(args.seconds), 0.5), 30);
    if (args.loop) body.loop = true;
    what = args.name || args.prompt;
    about = "sound effect";
  }
  kx.progress(`ElevenLabs: ${about}`);
  const res = kx.fetch(url, { method: "POST", headers, json: body });
  if (!res.ok) {
    let why = res.text();
    try {
      const d = res.json().detail;
      why = typeof d === "string" ? d : (d && d.message) || why;
    } catch (_) {}
    return kx.fail(`ElevenLabs said ${res.status}: ${why}`);
  }
  const file = kx.writeFile(`${kx.setting("output_folder")}/${kx.slug(what)}.mp3`, res.bytes());
  return { text: `Saved ${file} (${about}).`, files: [file] };
}
