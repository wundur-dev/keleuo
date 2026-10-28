// The model viewer's page script (runs in the web view or any browser, not in Keleuo's script
// engine). view_model writes MODEL_B64 (the GLB) and OPTIONS before it: a studio shot with a
// backdrop, a ground that catches the shadow, a key and a fill light, a three-quarter camera
// that can follow the body as it moves, and the model's animations to pick and play.
(function () {
  const $ = (id) => document.getElementById(id);
  const say = (text) => ($("status").textContent = text);

  const renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true });
  renderer.setPixelRatio(window.devicePixelRatio);
  renderer.outputEncoding = THREE.sRGBEncoding;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  $("stage").appendChild(renderer.domElement);

  const scene = new THREE.Scene();
  const light = new THREE.Color(0.92, 0.92, 0.94);
  const dark = new THREE.Color(0.11, 0.11, 0.13);
  scene.background = light.clone();

  const camera = new THREE.PerspectiveCamera(40, 1, 0.01, 1000);
  const controls = new THREE.OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;

  scene.add(new THREE.HemisphereLight(0xffffff, 0x8d8d8d, 0.9));
  const sun = new THREE.DirectionalLight(0xffffff, 2.2);
  sun.castShadow = true;
  sun.shadow.mapSize.set(2048, 2048);
  scene.add(sun, sun.target);
  const fill = new THREE.DirectionalLight(0xffffff, 0.6);
  scene.add(fill);

  // A checker ground, so the body is seen moving even when the camera follows it.
  const checker = document.createElement("canvas");
  checker.width = checker.height = 64;
  const g = checker.getContext("2d");
  for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) {
    g.fillStyle = (x + y) % 2 ? "#a9a9ad" : "#8e8e93";
    g.fillRect(x * 8, y * 8, 8, 8);
  }
  const texture = new THREE.CanvasTexture(checker);
  texture.wrapS = texture.wrapT = THREE.RepeatWrapping;
  texture.magFilter = THREE.NearestFilter;
  const ground = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), new THREE.MeshStandardMaterial({ map: texture, roughness: 0.95 }));
  ground.rotation.x = -Math.PI / 2;
  ground.receiveShadow = true;
  scene.add(ground);

  let mixer = null, action = null, clips = [], follow = null, size = 1, center = new THREE.Vector3(), playing = true, speed = 1;
  const clock = new THREE.Clock();

  function resize() {
    const w = $("stage").clientWidth, h = $("stage").clientHeight;
    renderer.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }
  window.addEventListener("resize", resize);

  // The three-quarter front view, the whole body in frame.
  function frame() {
    const dir = new THREE.Vector3(1, 0.35, 1).normalize();
    camera.position.copy(center).addScaledVector(dir, size * 2.1);
    controls.target.copy(center);
    camera.near = size / 100;
    camera.far = size * 100;
    camera.updateProjectionMatrix();
    controls.update();
  }

  function play(i) {
    if (!mixer) return;
    if (action) action.stop();
    action = mixer.clipAction(clips[i]);
    action.reset().play();
    action.paused = !playing;
    $("time").max = clips[i].duration;
  }

  function bytes(b64) {
    const raw = atob(b64);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out.buffer;
  }

  say("Loading the model…");
  new THREE.GLTFLoader().parse(bytes(MODEL_B64), "", (gltf) => {
    const model = gltf.scene;
    // Tripo's animated files can carry a stray default sphere, not part of the model.
    model.traverse((o) => {
      if (o.isMesh && /^icosphere/i.test(o.name)) o.visible = false;
      if (o.isMesh) {
        o.castShadow = true;
        o.receiveShadow = true;
        o.frustumCulled = false; // a skinned mesh's bounds don't follow its pose
      }
    });
    scene.add(model);

    const box = new THREE.Box3().setFromObject(model);
    box.getCenter(center);
    size = box.getSize(new THREE.Vector3()).length() || 1;
    ground.position.set(center.x, box.min.y, center.z);
    ground.scale.set(size * 20, size * 20, 1);
    texture.repeat.set(40, 40); // a square about a sixteenth of the model across
    sun.position.copy(center).add(new THREE.Vector3(-1.2, 2, 1.4).multiplyScalar(size));
    sun.target.position.copy(center);
    const s = size * 1.5;
    Object.assign(sun.shadow.camera, { left: -s, right: s, top: s, bottom: -s, near: size / 50, far: size * 10 });
    sun.shadow.camera.updateProjectionMatrix();
    fill.position.copy(center).add(new THREE.Vector3(2, 1, -1.5).multiplyScalar(size));

    // The bone the camera follows: the hips, as rigs name them (a root bone often stays put).
    const bones = [];
    model.traverse((o) => o.isBone && bones.push(o));
    for (const name of [/^(mixamorig:?)?hips?$/i, /^pelvis$/i, /hip/i, /^(bone)?root$/i]) {
      follow = bones.find((b) => name.test(b.name));
      if (follow) break;
    }

    clips = gltf.animations || [];
    let triangles = 0;
    model.traverse((o) => {
      if (o.isMesh && o.geometry) triangles += (o.geometry.index ? o.geometry.index.count : o.geometry.attributes.position.count) / 3;
    });
    $("info").textContent = `${OPTIONS.title} · ${Math.round(triangles).toLocaleString()} triangles · ${clips.length ? clips.length + " animation" + (clips.length > 1 ? "s" : "") : "no animations"}`;
    if (clips.length) {
      mixer = new THREE.AnimationMixer(model);
      clips.forEach((c, i) => $("clip").add(new Option(c.name || `Animation ${i + 1}`, i)));
      const wanted = clips.findIndex((c) => c.name.toLowerCase().includes(String(OPTIONS.animation || "").toLowerCase()));
      $("clip").value = OPTIONS.animation && wanted >= 0 ? wanted : 0;
      play(Number($("clip").value));
      $("anim").hidden = false;
    }
    $("followrow").hidden = !follow;
    // Aim at where the body is once its animation starts (a walk can begin away from the
    // model's resting place), not where it rests.
    if (mixer) mixer.update(0);
    model.updateMatrixWorld(true);
    if (follow) {
      const at = follow.getWorldPosition(new THREE.Vector3());
      center.x = at.x;
      center.z = at.z;
      ground.position.x = at.x;
      ground.position.z = at.z;
    }
    resize();
    frame();
    say("");
  }, (e) => say(`Couldn't read the model: ${e.message || e}`));

  $("clip").onchange = (e) => play(Number(e.target.value));
  $("play").onclick = () => {
    playing = !playing;
    if (action) action.paused = !playing;
    $("play").textContent = playing ? "Pause" : "Play";
  };
  $("speed").oninput = (e) => {
    speed = Number(e.target.value);
    $("speedv").textContent = `${speed.toFixed(2)}×`;
  };
  $("time").oninput = (e) => {
    if (!action) return;
    action.time = Number(e.target.value);
    mixer.update(0);
  };
  $("spin").onchange = (e) => (controls.autoRotate = e.target.checked);
  $("reset").onclick = frame;
  $("dark").onchange = (e) => {
    scene.background = (e.target.checked ? dark : light).clone();
    document.body.classList.toggle("dark", e.target.checked);
  };
  $("floor").onchange = (e) => (ground.visible = e.target.checked);
  $("save").onclick = () => {
    const a = document.createElement("a");
    a.download = `${OPTIONS.title}.png`;
    a.href = renderer.domElement.toDataURL("image/png");
    a.click();
  };

  renderer.setAnimationLoop(() => {
    const dt = clock.getDelta();
    if (mixer && playing) mixer.update(dt * speed);
    if (action) $("time").value = action.time;
    // Following: the camera keeps its place relative to the hips (a tracking shot, back with
    // them when the walk loops), at whatever angle it was turned to.
    if (follow && $("follow").checked) {
      const at = follow.getWorldPosition(new THREE.Vector3());
      const offset = camera.position.clone().sub(controls.target);
      controls.target.set(at.x, controls.target.y, at.z);
      camera.position.copy(controls.target).add(offset);
      ground.position.x = at.x - ((at.x % (size / 2)) + size / 2) % (size / 2);
      ground.position.z = at.z - ((at.z % (size / 2)) + size / 2) % (size / 2);
    }
    controls.update();
    renderer.render(scene, camera);
  });
})();
