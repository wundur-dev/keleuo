# Proposal: tool packs

Status: proposal for the owner to read; a first prototype follows it. Written 2026-10-28.

## The idea in one paragraph

A **tool pack** is a small download (a manifest and a JavaScript file or two) that gives every
agent new tools: "make an image with gpt-image", "make a sound effect with ElevenLabs", "turn
this image into a 3D model with Tripo". Keleuo runs the pack's scripts itself, in an embedded
JavaScript engine, so nothing needs installing (no Python, no Node, no `npm install`), and the
scripts can do only what the pack declared: reach the hosts it named, read the secrets it named
(from the system's key store), write inside the project. Agents get the tools the same way they
get your own (Settings > Tools): through Keleuo's own MCP server, `keleuo-mcp`, so a pack starts no
process of its own.

## What it is for

1. **Game assets, the first pack.** Images (OpenAI's gpt-image, directly or through Azure
   OpenAI), sound effects, voice lines and music (ElevenLabs), 3D models from text or an image
   (Tripo). The agent asks for "a 512x512 coin icon, transparent", the pack calls the API with
   your key and saves `assets/generated/coin.png`, and the agent sees the image.
2. **Any API an agent should be able to call with your key,** without the key ever being in a
   file, an environment variable it can read, or the conversation.
3. **Packs written by agents.** JavaScript is what models write most fluently: "make a pack for
   the Replicate API" is a task an agent can do in a session, tested with the pack's own tools.

## Why not each vendor's MCP server, or a Python script

Both work today (Settings > MCP servers, Settings > Tools), and stay. Packs exist for what they
cost:

- An MCP server is a process **per agent**: twelve sessions, twelve copies, each with its own
  runtime (often `npx` or `uvx` fetching packages at start). Pack tools run in the window, once.
- A Python or Node script needs that language, at the right version, with its packages. Many
  people have neither, and "works on my machine" is the first support request.
- A script on the machine can do anything you can: read your home folder, send your keys
  anywhere. A pack's script can't (see the sandbox below).

Vendors' official SDKs don't run in the embedded engine (they are npm or pip packages). Packs call
the REST APIs with `fetch`, which for these vendors is a few requests each.

## A pack

A folder (or a `.zip` of one):

```
game-assets/
  pack.json        the manifest
  image.js         one script per tool, or one script for all
  sound.js
  model3d.js
  README.md        shown in Settings
  icon.png         optional
```

`pack.json`:

```json
{
  "id": "wundur/game-assets",
  "name": "Game assets",
  "version": "0.1.0",
  "description": "Images, sound effects, voice and 3D models for games, with your own keys.",
  "homepage": "https://github.com/wundur-dev/keleuo-packs",
  "keleuo": ">=0.2.0",

  "secrets": [
    { "name": "OPENAI_API_KEY", "label": "OpenAI API key", "help": "https://platform.openai.com/api-keys", "optional": true },
    { "name": "AZURE_OPENAI_KEY", "label": "Azure OpenAI key", "optional": true },
    { "name": "ELEVENLABS_API_KEY", "label": "ElevenLabs API key", "optional": true },
    { "name": "TRIPO_API_KEY", "label": "Tripo API key", "optional": true }
  ],
  "settings": [
    { "name": "azure_endpoint", "label": "Azure OpenAI endpoint", "placeholder": "https://<name>.openai.azure.com" },
    { "name": "azure_deployment", "label": "Azure gpt-image deployment" },
    { "name": "image_provider", "label": "Images from", "choices": ["openai", "azure"], "default": "openai" },
    { "name": "output_folder", "label": "Save to", "default": "assets/generated" }
  ],
  "network": ["api.openai.com", "*.openai.azure.com", "api.elevenlabs.io", "api.tripo3d.ai", "*.tripo3d.com"],

  "tools": [
    {
      "name": "make_image",
      "description": "Make an image with gpt-image and save it in the project. Use for sprites, icons, textures, concept art. Returns the image.",
      "script": "image.js",
      "params": [
        { "name": "prompt", "kind": "text", "required": true, "description": "What to draw, in detail." },
        { "name": "size", "kind": "choice", "choices": ["1024x1024", "1536x1024", "1024x1536", "auto"], "description": "Pixel size." },
        { "name": "transparent", "kind": "flag", "description": "A transparent background (sprites, icons)." },
        { "name": "name", "kind": "text", "description": "File name to save as, without extension." }
      ],
      "needs": ["OPENAI_API_KEY|AZURE_OPENAI_KEY"],
      "cost": "about $0.04 to $0.17 an image, by size and quality",
      "ask": true
    }
  ]
}
```

Parameters use the same kinds as your own tools (text, number, yes/no, choice), so one schema and
one form serve both. `needs` says which secrets a tool can't run without (`|`: either one); a
tool whose needs are missing isn't offered to agents, and Settings says which key to add.

## The script

One function, given the call's arguments and `kx`, Keleuo's API; it returns what the agent gets.

```js
// image.js
export default async function (args, kx) {
  const azure = kx.setting("image_provider") === "azure";
  const body = {
    model: "gpt-image-1",
    prompt: args.prompt,
    size: args.size ?? "1024x1024",
    background: args.transparent ? "transparent" : "auto",
  };
  const url = azure
    ? `${kx.setting("azure_endpoint")}/openai/deployments/${kx.setting("azure_deployment")}/images/generations?api-version=2025-04-01-preview`
    : "https://api.openai.com/v1/images/generations";
  const headers = azure
    ? { "api-key": kx.secret("AZURE_OPENAI_KEY") }
    : { authorization: `Bearer ${kx.secret("OPENAI_API_KEY")}` };
  const res = await kx.fetch(url, { method: "POST", headers, json: body });
  if (!res.ok) return kx.fail(`The image service said ${res.status}: ${await res.text()}`);
  const data = await res.json();
  const file = await kx.writeFile(`${kx.setting("output_folder")}/${args.name ?? kx.slug(args.prompt)}.png`, kx.base64(data.data[0].b64_json));
  return { text: `Saved ${file}`, images: [file] };
}
```

### `kx`, all a script can do

| Call | What it does | Limits |
|---|---|---|
| `kx.fetch(url, { method, headers, body \| json \| form })` | an HTTP request; `res.ok`, `res.status`, `res.json()`, `res.text()`, `res.bytes()` | only to the manifest's `network` hosts, HTTPS only; 100 MB per response |
| `kx.secret(name)` | a secret's value, from the key store | only names in the manifest; never returned to the agent or written to the log |
| `kx.setting(name)` | a pack setting (Settings > Tools) | |
| `kx.writeFile(path, bytes \| text)` | write in the session's folder; returns the path written (a free name if taken) | inside the project only, as the write sandbox |
| `kx.readFile(path)` | read a project file (an image to turn into a 3D model) | inside the project only |
| `kx.sleep(ms)`, `kx.progress(text)` | wait (polling a job), say how far it is (shown on the tool call's card) | |
| `kx.base64(text)`, `kx.slug(text)`, `kx.fail(why)` | helpers | |

Nothing else: no file access outside the project, no processes, no other hosts, no environment
variables, no other packs' secrets. The engine has none of these of its own; Keleuo adds only
the calls above.

### Results

`{ text, images: [paths], files: [paths] }`. Images go back to the agent as images (as your own
tools' do now), so it can look at what it made and try again. `files` (a sound, a model) are
named in the text, and shown on the tool call's card with a preview (see Previews).

## Safety

A pack is downloaded code that holds your keys and can spend your money. The rules:

1. **Declared powers, shown at install.** Installing shows, in plain words: the hosts it may
   reach, the secrets it may read, that it writes in the project. A new version that asks for
   more asks again.
2. **Keys stay out of reach.** A secret leaves the key store only into that pack's script, only
   for its declared hosts' requests in practice (it can't be written to a file, logged or
   returned: Keleuo blanks it if it appears in a result or a file the script writes).
3. **Ask before paid calls.** Tools with a `cost` ask before each run by default (the card shows
   the call and the cost note: Allow, Always for this session, Deny), as your own tools can now.
   You can turn it off per tool.
4. **Limits.** Each run has a time limit (default two minutes; a job that polls can declare
   longer, up to ten), a memory limit (64 MB in the engine), and can be stopped from its card.
5. **Pinned versions.** A pack is installed at a version and its hash; updates are offered, never
   applied silently.
6. **Packs in a repo don't run on their own.** A project can list packs it uses
   (`.keleuo/packs.json`), but they install only when you say so, as project scripts in the
   scripting proposal.

## Where things live

- Installed packs: `~/Library/Application Support/keleuo/packs/<id>/<version>/` (and the same
  folder under Linux's and Windows' config dirs).
- Secrets: the system key store, as `pack/<id>/<NAME>`. macOS: Keychain (now). Linux: Secret
  Service through `secret-tool` (now). **Windows: Credential Manager, to add** before packs work
  there.
- Settings > Tools gets a **Packs** section beside your own tools: installed packs with their
  tools and switches (per app or per folder, as tools and MCP servers have), their keys and
  settings, Install from a link or a folder, Update, Remove.

## Previews (separate work, useful without packs)

What packs make should be visible in Keleuo:

- **Audio:** `.wav`, `.mp3`, `.ogg`, `.m4a` play in the file viewer and on the tool call's card.
- **3D models:** `.glb` and `.gltf` in a viewer you can turn, drawn by the built-in web view with
  Google's `<model-viewer>`, bundled (no network).
- **A gallery:** a folder's images, sounds and models as a grid, for picking from variations.

## The first pack: game assets

| Tool | Service | Notes |
|---|---|---|
| `make_image` | OpenAI `gpt-image-1`, or Azure OpenAI | size, transparent background, quality; saves PNG |
| `edit_image` | the same | change an existing image with a prompt (and a mask), e.g. recolour a sprite |
| `sound_effect` | ElevenLabs sound generation | text and length; saves MP3 |
| `voice_line` | ElevenLabs text to speech | text and voice; saves MP3 |
| `music` | ElevenLabs music | prompt and length |
| `model_from_text` / `model_from_image` | Tripo | a job: starts it, polls, downloads the GLB (and its preview image, returned to the agent) |

Each service's request details are checked against its documentation when the tool is written
(models and endpoints change; that is also why packs update apart from Keleuo).

## Phases

1. **The engine and one tool** (the prototype): QuickJS in the window, `kx` with fetch, secret,
   setting, writeFile; a pack installed from a folder; its tools listed by `keleuo-mcp` beside your
   own; `make_image` working end to end with an OpenAI key.
2. **Packs in Settings:** install from a link or zip, keys and settings forms, per-folder
   switches, ask-before-run with cost, updates.
3. **The game-assets pack complete:** ElevenLabs and Tripo tools, Azure for images.
4. **Previews:** audio, 3D, gallery.
5. **Windows key store,** then packs on Windows and Linux.

## Questions for you

1. Should packs live in a public `wundur-dev/keleuo-packs` repo (others can send theirs), or stay
   yours for now?
2. Is ask-before-run on paid tools the right default, or too much friction for asset work, where
   you might make thirty images in a row? (A middle way: ask once per session per tool.)
3. Where should generated assets go by default: `assets/generated/`, or a folder you pick per
   project?
