(() => {
  'use strict';
  const C = __VMCTL_BROWSER_PROFILE__;
  const N = C.navigator || {};
  const W = C.webgl || {};
  const seedString = String(C.seed || '0');

  function hash32(value) {
    let x = value >>> 0;
    x = Math.imul(x ^ (x >>> 16), 0x45d9f3b);
    x = Math.imul(x ^ (x >>> 16), 0x45d9f3b);
    return (x ^ (x >>> 16)) >>> 0;
  }
  function seedNumber(text) {
    let h = 2166136261;
    for (let i = 0; i < text.length; i++) {
      h ^= text.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }
  const canvasSeed = seedNumber(String(C.canvas && C.canvas.seed ? C.canvas.seed : seedString));
  function noiseAt(index, salt) {
    const h = hash32((index + salt) ^ canvasSeed);
    return ((h / 4294967295) * 2 - 1) * Number((C.canvas && C.canvas.noise) || 0);
  }
  function define(target, prop, getter) {
    try {
      Object.defineProperty(target, prop, { get: getter, configurable: true, enumerable: true });
      return true;
    } catch (e) {
      return false;
    }
  }
  function defineValue(target, prop, value) {
    try {
      Object.defineProperty(target, prop, { value: value, configurable: true, writable: true });
    } catch (e) {}
  }

  // ---- navigator ---------------------------------------------------------
  define(Navigator.prototype, 'userAgent', () => N.user_agent);
  define(Navigator.prototype, 'appVersion', () => N.app_version || N.user_agent);
  define(Navigator.prototype, 'platform', () => N.platform);
  define(Navigator.prototype, 'vendor', () => N.vendor);
  define(Navigator.prototype, 'language', () => N.language || (N.languages || [])[0] || 'en-US');
  define(Navigator.prototype, 'languages', () => (N.languages || []).slice());
  define(Navigator.prototype, 'hardwareConcurrency', () => N.hardware_concurrency);
  define(Navigator.prototype, 'deviceMemory', () => N.device_memory);
  define(Navigator.prototype, 'maxTouchPoints', () => N.max_touch_points || 0);
  define(Navigator.prototype, 'webdriver', () => !!N.webdriver);
  if (N.do_not_track !== null && N.do_not_track !== undefined) {
    define(Navigator.prototype, 'doNotTrack', () => String(N.do_not_track));
  }
  if (C.ua_data) {
    const ua = C.ua_data;
    const data = {
      brands: ua.brands,
      mobile: ua.mobile,
      platform: ua.platform,
      getHighEntropyValues(hints) {
        const map = {
          architecture: ua.architecture,
          bitness: ua.bitness,
          model: ua.model,
          platformVersion: ua.platform_version,
          uaFullVersion: ua.full_version,
          fullVersionList: ua.full_version_list,
          wow64: ua.wow64,
          formFactors: ['Desktop'],
        };
        const out = { brands: ua.brands, mobile: ua.mobile, platform: ua.platform };
        for (const hint of (hints || [])) {
          if (Object.prototype.hasOwnProperty.call(map, hint)) out[hint] = map[hint];
        }
        return Promise.resolve(out);
      },
      toJSON() { return { brands: ua.brands, mobile: ua.mobile, platform: ua.platform }; }
    };
    define(Navigator.prototype, 'userAgentData', () => data);
  }
  function pluginArray(items) {
    const arr = {
      length: items.length,
      item: (i) => (i >= 0 && i < items.length ? arr[i] : null),
      namedItem: (name) => {
        for (let i = 0; i < items.length; i++) if (items[i].name === name) return items[i];
        return null;
      },
      refresh: () => {}
    };
    for (let i = 0; i < items.length; i++) arr[i] = items[i];
    return arr;
  }
  if (Array.isArray(N.plugins)) {
    const plugs = N.plugins.map((name) => ({
      name: name, filename: 'internal-pdf-viewer', description: 'Portable Document Format',
      length: 1, 0: { type: 'application/pdf', suffixes: 'pdf', description: 'Portable Document Format', enabledPlugin: null }
    }));
    const pa = pluginArray(plugs);
    define(Navigator.prototype, 'plugins', () => pa);
  }
  if (Array.isArray(N.mime_types)) {
    const mimes = N.mime_types.map((type) => ({
      type: type, suffixes: 'pdf', description: 'Portable Document Format', enabledPlugin: null
    }));
    define(Navigator.prototype, 'mimeTypes', () => pluginArray(mimes));
  }
  if (N.connection) {
    define(Navigator.prototype, 'connection', () => ({
      effectiveType: N.connection.effective_type,
      rtt: N.connection.rtt,
      downlink: N.connection.downlink,
      saveData: N.connection.save_data,
      onchange: null
    }));
  }
  if (C.battery) {
    define(Navigator.prototype, 'getBattery', () => function () {
      return Promise.resolve({
        charging: !!C.battery.charging,
        chargingTime: C.battery.charging ? 0 : Infinity,
        dischargingTime: C.battery.charging ? Infinity : 0,
        level: Number(C.battery.level),
        onchargingchange: null, onchargingtimechange: null,
        ondischargingtimechange: null, onlevelchange: null,
        addEventListener: () => {}, removeEventListener: () => {}
      });
    });
  }
  if (C.media_devices && navigator.mediaDevices) {
    const count = Math.max(0, Number(C.media_devices.count) || 0);
    const devices = [];
    for (let i = 0; i < count; i++) {
      devices.push({ deviceId: 'default', kind: 'audioinput', label: '', groupId: 'default' });
      devices.push({ deviceId: 'default', kind: 'audiooutput', label: '', groupId: 'default' });
      devices.push({ deviceId: 'default', kind: 'videoinput', label: '', groupId: 'default' });
    }
    try {
      Object.defineProperty(navigator.mediaDevices, 'enumerateDevices', {
        value: function () { return Promise.resolve(devices.slice()); },
        configurable: true
      });
    } catch (e) {}
  }
  if (C.permissions) {
    const origQuery = Permissions.prototype.query;
    Permissions.prototype.query = function (desc) {
      const name = desc && desc.name;
      if (name === 'geolocation') return Promise.resolve({ state: C.permissions.geolocation || 'denied', onchange: null });
      if (name === 'notifications') return Promise.resolve({ state: C.permissions.notifications || 'denied', onchange: null });
      return origQuery.apply(this, arguments);
    };
  }
  if (navigator.geolocation) {
    const denied = { code: 1, message: 'User denied Geolocation', PERMISSION_DENIED: 1, POSITION_UNAVAILABLE: 2, TIMEOUT: 3 };
    try {
      Object.defineProperty(navigator.geolocation, 'getCurrentPosition', {
        value: function (success, error) { if (typeof error === 'function') error(denied); },
        configurable: true
      });
      Object.defineProperty(navigator.geolocation, 'watchPosition', {
        value: function () { return 0; }, configurable: true
      });
    } catch (e) {}
  }
  if (typeof Notification !== 'undefined') {
    const state = (C.permissions && C.permissions.notifications) || 'denied';
    define(Notification, 'permission', () => state);
    try {
      Notification.requestPermission = function () { return Promise.resolve(state); };
    } catch (e) {}
  }

  // ---- screen ------------------------------------------------------------
  if (C.screen && !(C.native_display && C.native_display.enabled)) {
    const S = C.screen;
    const props = {
      width: Number(S.width), height: Number(S.height),
      availWidth: Number(S.avail_width), availHeight: Number(S.avail_height),
      colorDepth: Number(S.color_depth), pixelDepth: Number(S.pixel_depth)
    };
    for (const key of Object.keys(props)) {
      if (!define(window.screen, key, () => props[key])) define(Screen.prototype, key, () => props[key]);
    }
    define(window, 'devicePixelRatio', () => Number(S.device_pixel_ratio));
  }

  // ---- timezone / Intl fallback -----------------------------------------
  if (C.timezone) {
    try {
      const Orig = Intl.DateTimeFormat;
      function Patched(locale, options) {
        const opts = Object.assign({}, options || {});
        if (!opts.timeZone) opts.timeZone = C.timezone;
        return new Orig(locale, opts);
      }
      Patched.prototype = Orig.prototype;
      Object.setPrototypeOf(Patched, Orig);
      Patched.supportedLocalesOf = Orig.supportedLocalesOf.bind(Orig);
      Intl.DateTimeFormat = Patched;
    } catch (e) {}
  }

  // ---- canvas ------------------------------------------------------------
  const origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
  const origPutImageData = CanvasRenderingContext2D.prototype.putImageData;
  function perturb(imageData) {
    const data = imageData && imageData.data;
    if (!data || !Number((C.canvas && C.canvas.noise) || 0)) return imageData;
    for (let i = 0; i < data.length; i += 4) {
      data[i] = Math.max(0, Math.min(255, data[i] + noiseAt(i, 1)));
      data[i + 1] = Math.max(0, Math.min(255, data[i + 1] + noiseAt(i + 1, 2)));
      data[i + 2] = Math.max(0, Math.min(255, data[i + 2] + noiseAt(i + 2, 3)));
    }
    return imageData;
  }
  CanvasRenderingContext2D.prototype.getImageData = function () {
    return perturb(origGetImageData.apply(this, arguments));
  };
  const origToDataURL = HTMLCanvasElement.prototype.toDataURL;
  HTMLCanvasElement.prototype.toDataURL = function () {
    try {
      const w = this.width, h = this.height;
      if (w > 0 && h > 0) {
        const copy = document.createElement('canvas');
        copy.width = w; copy.height = h;
        const copyCtx = copy.getContext('2d');
        const ctx = this.getContext('2d');
        if (copyCtx && ctx) {
          copyCtx.putImageData(perturb(origGetImageData.call(ctx, 0, 0, w, h)), 0, 0);
          return origToDataURL.apply(copy, arguments);
        }
      }
    } catch (e) {}
    return origToDataURL.apply(this, arguments);
  };
  const origToBlob = HTMLCanvasElement.prototype.toBlob;
  HTMLCanvasElement.prototype.toBlob = function (callback, type, quality) {
    try {
      const w = this.width, h = this.height;
      if (w > 0 && h > 0) {
        const copy = document.createElement('canvas');
        copy.width = w; copy.height = h;
        const copyCtx = copy.getContext('2d');
        const ctx = this.getContext('2d');
        if (copyCtx && ctx) {
          copyCtx.putImageData(perturb(origGetImageData.call(ctx, 0, 0, w, h)), 0, 0);
          return origToBlob.call(copy, callback, type, quality);
        }
      }
    } catch (e) {}
    return origToBlob.call(this, callback, type, quality);
  };
  if (typeof OffscreenCanvas !== 'undefined' && OffscreenCanvas.prototype.convertToBlob) {
    const origConvert = OffscreenCanvas.prototype.convertToBlob;
    OffscreenCanvas.prototype.convertToBlob = function () {
      try {
        const w = this.width, h = this.height;
        const ctx = this.getContext('2d');
        if (ctx && w > 0 && h > 0) {
          const copy = new OffscreenCanvas(w, h);
          const copyCtx = copy.getContext('2d');
          const data = origGetImageData.call(ctx, 0, 0, w, h);
          perturb(data);
          copyCtx.putImageData(data, 0, 0);
          return origConvert.apply(copy, arguments);
        }
      } catch (e) {}
      return origConvert.apply(this, arguments);
    };
  }

  // ---- WebGL -------------------------------------------------------------
  function patchGL(proto) {
    if (!proto) return;
    const origGetParameter = proto.getParameter;
    if (origGetParameter) {
      proto.getParameter = function (p) {
        const key = String(p);
        if (p === 37445 || p === 7936) return W.vendor;
        if (p === 37446 || p === 7937) return W.renderer;
        if (p === 7938) return W.version;
        if (p === 35724) return W.shading_language_version;
        if (W.parameters && Object.prototype.hasOwnProperty.call(W.parameters, key)) {
          const value = W.parameters[key];
          return Array.isArray(value) ? new Int32Array(value) : value;
        }
        return origGetParameter.call(this, p);
      };
    }
    if (typeof W.extensions !== 'undefined' && W.extensions !== null) {
      const origGetExtension = proto.getExtension;
      proto.getExtension = function (name) {
        if (!W.extensions.includes(name) && name !== 'WEBGL_debug_renderer_info') return null;
        return origGetExtension.apply(this, arguments);
      };
      if (proto.getSupportedExtensions) {
        proto.getSupportedExtensions = function () { return W.extensions.slice(); };
      }
    }
    if (Number(W.read_pixels_noise || 0) > 0 && proto.readPixels) {
      const origRead = proto.readPixels;
      proto.readPixels = function (x, y, w, h, fmt, type, pixels, offset) {
        origRead.apply(this, arguments);
        if (pixels && pixels.length) {
          const amp = Number(W.read_pixels_noise || 0);
          for (let i = 0; i < pixels.length; i++) {
            pixels[i] = Math.max(0, Math.min(255, pixels[i] + ((hash32(i ^ canvasSeed) / 4294967295) * 2 - 1) * amp));
          }
        }
      };
    }
  }
  if (typeof WebGLRenderingContext !== 'undefined') patchGL(WebGLRenderingContext.prototype);
  if (typeof WebGL2RenderingContext !== 'undefined') patchGL(WebGL2RenderingContext.prototype);

  // ---- audio -------------------------------------------------------------
  if (Number((C.audio && C.audio.noise) || 0) > 0) {
    const audioNoise = Number(C.audio.noise);
    const origChannel = AudioBuffer.prototype.getChannelData;
    AudioBuffer.prototype.getChannelData = function () {
      const data = origChannel.apply(this, arguments);
      for (let i = 0; i < data.length; i++) {
        data[i] += ((hash32(i ^ canvasSeed) / 4294967295) * 2 - 1) * audioNoise;
      }
      return data;
    };
    const origFloatFreq = AnalyserNode.prototype.getFloatFrequencyData;
    AnalyserNode.prototype.getFloatFrequencyData = function (array) {
      origFloatFreq.apply(this, arguments);
      for (let i = 0; i < array.length; i++) array[i] += ((hash32(i ^ canvasSeed) / 4294967295) * 2 - 1) * audioNoise;
    };
    const origByteFreq = AnalyserNode.prototype.getByteFrequencyData;
    AnalyserNode.prototype.getByteFrequencyData = function (array) {
      origByteFreq.apply(this, arguments);
      for (let i = 0; i < array.length; i++) array[i] = Math.max(0, Math.min(255, array[i] + Math.round(((hash32(i ^ canvasSeed) / 4294967295) * 2 - 1) * audioNoise * 255)));
    };
  }

  // ---- fonts / rects -----------------------------------------------------
  if (C.fonts) {
    if (C.fonts.check === true && typeof FontFaceSet !== 'undefined') {
      const origCheck = FontFaceSet.prototype.check;
      FontFaceSet.prototype.check = function () {
        if (C.fonts.check) return true;
        return origCheck.apply(this, arguments);
      };
    }
    const measureNoise = Number(C.fonts.measure_noise || 0);
    if (measureNoise) {
      const origMeasure = CanvasRenderingContext2D.prototype.measureText;
      CanvasRenderingContext2D.prototype.measureText = function (text) {
        const result = origMeasure.apply(this, arguments);
        try {
          Object.defineProperty(result, 'width', {
            value: result.width + noiseAt(String(text).length, 9) * measureNoise,
            configurable: true
          });
        } catch (e) {}
        return result;
      };
    }
  }
  if (Number((C.rects && C.rects.noise) || 0) > 0) {
    const origRect = Element.prototype.getBoundingClientRect;
    Element.prototype.getBoundingClientRect = function () {
      const r = origRect.apply(this, arguments);
      const n = Number(C.rects.noise);
      return new DOMRect(r.x + noiseAt(1, 11) * n, r.y + noiseAt(2, 12) * n,
                         r.width + noiseAt(3, 13) * n, r.height + noiseAt(4, 14) * n);
    };
  }

  // ---- WebRTC ------------------------------------------------------------
  if (C.webrtc && C.webrtc.js_filter && typeof RTCPeerConnection !== 'undefined') {
    const OrigPC = RTCPeerConnection;
    const Patched = function () {
      const pc = new OrigPC(...arguments);
      const origAdd = pc.addEventListener.bind(pc);
      pc.addEventListener = function (type, listener, options) {
        if (type === 'icecandidate' && typeof listener === 'function') {
          const wrapped = function (event) {
            try {
              const c = event.candidate;
              if (c && /([0-9]{1,3}\.){3}[0-9]{1,3}|\.local/.test(c.candidate || '')) return;
              return listener.call(pc, event);
            } catch (e) {}
          };
          return origAdd(type, wrapped, options);
        }
        return origAdd(type, listener, options);
      };
      return pc;
    };
    Patched.prototype = OrigPC.prototype;
    Object.setPrototypeOf(Patched, OrigPC);
    window.RTCPeerConnection = Patched;
  }

  // ---- worker shadow (best effort) --------------------------------------
  // Page.addScriptToEvaluateOnNewDocument does not reach dedicated workers;
  // workers created by the page inherit their own realm.  This hook makes the
  // common same-origin Worker constructor fail closed only when the profile
  // asks for it (disabled by default to avoid breaking applications).
  if (C.worker_block === true && typeof Worker !== 'undefined') {
    const OrigWorker = Worker;
    window.Worker = function () { throw new Error('Worker disabled by profile'); };
    window.Worker.prototype = OrigWorker.prototype;
  }
})();
