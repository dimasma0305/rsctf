/**
 * Procedural Web Audio engine for the battle arena — extracted from Attack.tsx.
 *
 * Fully self-contained: it owns its own AudioContext / master gain / reverb and an
 * enabled flag, has ZERO DOM/canvas/game-state dependencies, and exposes only the
 * sfx surface + lifecycle (unlock / resume / setEnabled / close). Every sound is
 * synthesised on the fly, so the arena does not ship third-party audio samples.
 */

export interface SoundEngine {
  unlock(): void
  resume(): void
  setEnabled(on: boolean): void
  isEnabled(): boolean
  close(): void
  sfxAttack(): void
  sfxDefend(): void
  sfxDown(): void
  sfxCapture(): void
  sfxPatch(): void
  sfxFreeze(): void
  sfxVictory(): void
  sfxNeutral(): void
  sfxMiss(): void
  sfxMumble(): void
  sfxRound(): void
  sfxUnfreeze(): void
  sfxSolve(): void
  sfxIncoming(durationSeconds?: number): () => void
  sfxFirstBlood(): () => void
}

export function createSoundEngine(): SoundEngine {
  const MASTER_LEVEL = 1.6
  let AC: any = null, masterGain: any = null, reverb: any = null, enabled = true, unlocked = false
  const activeStops = new Set<() => void>()
  const rng = (a: number, b: number) => a + Math.random() * (b - a)

  function ensureAudio(): any {
    try {
      if (!AC) {
        AC = new (window.AudioContext || (window as any).webkitAudioContext)()
        masterGain = AC.createGain(); masterGain.gain.value = enabled ? MASTER_LEVEL : 0
        const comp = AC.createDynamicsCompressor()
        comp.threshold.value = -16; comp.ratio.value = 12; comp.attack.value = 0.003; comp.release.value = 0.25
        masterGain.connect(comp); comp.connect(AC.destination)
      }
      return AC
    } catch (e) { return null }
  }
  function runningAudio(): any {
    if (!enabled || !unlocked) return null
    const ac = ensureAudio()
    return ac && ac.state === 'running' ? ac : null
  }
  function markRunning(ac: any) {
    if (ac === AC && ac.state === 'running') unlocked = true
  }
  function setMasterLevel(value: number) {
    if (!AC || !masterGain) return
    try {
      masterGain.gain.cancelScheduledValues(AC.currentTime)
      masterGain.gain.setValueAtTime(value, AC.currentTime)
    } catch (e) { masterGain.gain.value = value }
  }
  function trackStop(stop: () => void): () => void {
    let stopped = false
    const tracked = () => {
      if (stopped) return
      stopped = true
      activeStops.delete(tracked)
      stop()
    }
    activeStops.add(tracked)
    return tracked
  }
  function stopActive() {
    for (const stop of [...activeStops]) stop()
  }
  function makeReverb() {
    const ac = runningAudio(); if (!ac || reverb) return
    const len = Math.floor(ac.sampleRate * 2.8), buf = ac.createBuffer(2, len, ac.sampleRate)
    for (let ch = 0; ch < 2; ch++) { const d = buf.getChannelData(ch); for (let i = 0; i < len; i++) d[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / len, 2.8) }
    reverb = ac.createConvolver(); reverb.buffer = buf
    const rg = ac.createGain(); rg.gain.value = 0.95; reverb.connect(rg); rg.connect(masterGain)
  }
  function tone(o: any) {
    const ac = runningAudio(); if (!ac) return null; const t0 = ac.currentTime + (o.delay || 0)
    const osc = ac.createOscillator(); osc.type = o.type || 'sine'
    const g = ac.createGain(); const dur = o.dur || 0.15, vol = o.vol || 0.2, atk = o.attack || 0.005
    osc.frequency.setValueAtTime(o.f, t0)
    if (o.f2 != null) osc.frequency.exponentialRampToValueAtTime(Math.max(o.f2, 1), t0 + (o.glide || dur))
    g.gain.setValueAtTime(0.0001, t0)
    g.gain.exponentialRampToValueAtTime(vol, t0 + atk)
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + dur)
    osc.connect(g); g.connect(o.output || masterGain)
    if (o.rev && reverb) { const rs = ac.createGain(); rs.gain.value = o.rev; g.connect(rs); rs.connect(reverb) }
    osc.start(t0); osc.stop(t0 + dur + 0.03)
    return osc
  }
  function noiseBurst(o: any) {
    const ac = runningAudio(); if (!ac) return null; const t0 = ac.currentTime + (o.delay || 0)
    const dur = o.dur || 0.2, vol = o.vol || 0.2
    const n = ac.createBufferSource()
    const buf = ac.createBuffer(1, Math.max(1, Math.floor(ac.sampleRate * dur)), ac.sampleRate)
    const d = buf.getChannelData(0); for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1
    n.buffer = buf
    const filt = ac.createBiquadFilter(); filt.type = o.type || 'highpass'
    filt.frequency.setValueAtTime(o.f || 1000, t0); filt.Q.value = o.q || 1
    if (o.fEnd != null) filt.frequency.exponentialRampToValueAtTime(Math.max(o.fEnd, 1), t0 + dur)
    const g = ac.createGain()
    g.gain.setValueAtTime(0.0001, t0); g.gain.exponentialRampToValueAtTime(vol, t0 + 0.005)
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + dur)
    n.connect(filt); filt.connect(g); g.connect(o.output || masterGain)
    if (o.rev && reverb) { const rs = ac.createGain(); rs.gain.value = o.rev; g.connect(rs); rs.connect(reverb) }
    n.start(t0); n.stop(t0 + dur + 0.03)
    return n
  }
  const on = () => runningAudio() // every sfx gates on an explicitly unlocked context

  return {
    unlock() {
      const ac = ensureAudio()
      if (ac?.state === 'running') markRunning(ac)
      else if (ac?.state === 'suspended') {
        try { void Promise.resolve(ac.resume()).then(() => markRunning(ac)).catch(() => {}) } catch (e) {}
      }
      if ('speechSynthesis' in window) { try { speechSynthesis.getVoices() } catch (e) {} }
    },
    resume() {
      if (!unlocked || !AC || AC.state !== 'suspended') return
      try { void Promise.resolve(AC.resume()).then(() => markRunning(AC)).catch(() => {}) } catch (e) {}
    },
    setEnabled(v: boolean) {
      if (v === enabled) return
      enabled = v
      if (!enabled) { stopActive(); setMasterLevel(0) }
      else setMasterLevel(MASTER_LEVEL)
    },
    isEnabled() { return enabled },
    close() {
      stopActive()
      try { if (AC) AC.close() } catch (e) {}
      AC = null; masterGain = null; reverb = null; unlocked = false
    },
    sfxAttack() {
      if (!on()) return
      const p = Math.random(), d = rng(0.84, 1.2)
      if (p < 0.25) { tone({ type: 'square', f: 900 * d, f2: 170 * d, dur: 0.13, vol: 0.15 }); noiseBurst({ type: 'highpass', f: 2200, fEnd: 500, dur: 0.09, vol: 0.05 }) }
      else if (p < 0.5) { tone({ type: 'triangle', f: 1300 * d, f2: 320 * d, dur: 0.11, vol: 0.15 }); tone({ type: 'square', f: 650 * d, f2: 200 * d, dur: 0.08, vol: 0.07, delay: 0.012 }) }
      else if (p < 0.75) { noiseBurst({ type: 'bandpass', f: 3200 * d, fEnd: 700, dur: 0.16, vol: 0.15, q: 1.2 }); tone({ type: 'sine', f: 520 * d, f2: 240 * d, dur: 0.1, vol: 0.06 }) }
      else { tone({ type: 'square', f: 520 * d, f2: 520 * d, dur: 0.05, vol: 0.12 }); tone({ type: 'square', f: 780 * d, f2: 300 * d, dur: 0.09, vol: 0.11, delay: 0.05 }) }
    },
    sfxDefend() {
      if (!on()) return
      tone({ type: 'sine', f: 440, f2: 880, dur: 0.18, vol: 0.15, attack: 0.01 })
      tone({ type: 'triangle', f: 660, f2: 1320, dur: 0.22, vol: 0.11, delay: 0.04 })
      noiseBurst({ type: 'highpass', f: 4000, fEnd: 9000, dur: 0.18, vol: 0.05, delay: 0.02 })
    },
    sfxDown() {
      if (!on()) return
      tone({ type: 'sawtooth', f: 300, f2: 58, dur: 0.4, vol: 0.17 })
      tone({ type: 'square', f: 160, f2: 46, dur: 0.45, vol: 0.11, delay: 0.02 })
      noiseBurst({ type: 'lowpass', f: 1200, fEnd: 200, dur: 0.3, vol: 0.13 })
      for (let i = 0; i < 4; i++) tone({ type: 'square', f: rng(120, 400), f2: rng(80, 200), dur: 0.03, vol: 0.06, delay: 0.05 + i * 0.04 })
    },
    sfxCapture() {
      if (!on()) return
      ;[392, 523, 659, 784].forEach((f, i) => tone({ type: 'triangle', f, f2: f, dur: 0.16, vol: 0.12, delay: i * 0.05 }))
      noiseBurst({ type: 'highpass', f: 3000, fEnd: 8000, dur: 0.2, vol: 0.05, delay: 0.05 })
    },
    sfxPatch() {
      if (!on()) return
      tone({ type: 'square', f: 360, f2: 520, dur: 0.05, vol: 0.13 })
      tone({ type: 'square', f: 520, f2: 720, dur: 0.05, vol: 0.13, delay: 0.06 })
      tone({ type: 'triangle', f: 900, f2: 1500, dur: 0.18, vol: 0.11, delay: 0.13 })
      noiseBurst({ type: 'highpass', f: 5000, fEnd: 9000, dur: 0.1, vol: 0.045, delay: 0.13 })
    },
    sfxFreeze() {
      if (!on()) return
      makeReverb()
      ;[1568, 1318, 1046, 880].forEach((f, i) => tone({ type: 'sine', f, f2: f, dur: 0.3, vol: 0.08, delay: i * 0.06, rev: 0.5 }))
      noiseBurst({ type: 'highpass', f: 8000, fEnd: 3000, dur: 0.6, vol: 0.06, rev: 0.6 })
      tone({ type: 'sine', f: 200, f2: 80, dur: 0.5, vol: 0.12, delay: 0.1 })
    },
    sfxVictory() {
      if (!on()) return
      makeReverb()
      ;[392, 523, 659, 784, 1046].forEach((f, i) => tone({ type: 'triangle', f, f2: f, dur: 0.5, vol: 0.16, delay: i * 0.12, rev: 0.4 }))
      ;[523, 659, 784].forEach((f) => tone({ type: 'sawtooth', f, f2: f, dur: 1.3, vol: 0.08, delay: 0.62, rev: 0.5 }))
      tone({ type: 'sine', f: 130, f2: 64, dur: 1.5, vol: 0.5, delay: 0.58 })
      noiseBurst({ type: 'highpass', f: 5000, fEnd: 9000, dur: 1.7, vol: 0.18, delay: 0.58, rev: 0.85 })
    },
    sfxNeutral() { // a hill lost / went neutral — two descending sines
      if (!on()) return
      tone({ type: 'sine', f: 660, f2: 330, dur: 0.18, vol: 0.1 })
      tone({ type: 'sine', f: 440, f2: 220, dur: 0.22, vol: 0.08, delay: 0.06 })
    },
    sfxMiss() { // a rejected / wrong flag — soft low downbend (reads as a non-event)
      if (!on()) return
      tone({ type: 'sine', f: 380, f2: 240, dur: 0.12, vol: 0.07 })
    },
    sfxMumble() { // a service slipped to MUMBLE — short warning chirp
      if (!on()) return
      tone({ type: 'square', f: 520, f2: 600, dur: 0.06, vol: 0.08 })
      noiseBurst({ type: 'bandpass', f: 1800, dur: 0.08, vol: 0.04 })
    },
    sfxRound() { // round rollover — two-note ascending chime
      if (!on()) return
      ;[660, 990].forEach((f, i) => tone({ type: 'triangle', f, f2: f, dur: 0.12, vol: 0.07, delay: i * 0.07 }))
    },
    sfxUnfreeze() { // scoreboard unlocks — ascending mirror of sfxFreeze
      if (!on()) return
      makeReverb()
      ;[880, 1046, 1318, 1568].forEach((f, i) => tone({ type: 'sine', f, f2: f, dur: 0.22, vol: 0.07, delay: i * 0.05, rev: 0.4 }))
    },
    sfxSolve() { // a jeopardy challenge solved (laser hits the star) — bright chime
      if (!on()) return
      ;[784, 1046, 1318].forEach((f, i) => tone({ type: 'triangle', f, f2: f, dur: 0.13, vol: 0.11, delay: i * 0.05 }))
      noiseBurst({ type: 'highpass', f: 4500, fEnd: 9000, dur: 0.16, vol: 0.045, delay: 0.05 })
    },
    sfxIncoming(durationSeconds = 5) {
      const ac = on()
      if (!ac) return () => {}
      const duration = Math.min(8, Math.max(1, durationSeconds))
      const start = ac.currentTime + 0.02
      const bus = ac.createGain()
      bus.gain.setValueAtTime(0.0001, start)
      bus.gain.exponentialRampToValueAtTime(0.42, start + Math.max(0.1, duration - 0.18))
      bus.gain.exponentialRampToValueAtTime(0.0001, start + duration)
      bus.connect(masterGain)
      const sources: any[] = []
      const pulses = Math.max(4, Math.floor(duration * 2))
      for (let i = 0; i < pulses; i++) {
        const pulseAt = start + (i * duration) / pulses
        const pulseLength = Math.min(0.28, duration / pulses * 0.62)
        const baseFrequency = 220 + i * 18
        for (const [frequency, volume] of [[baseFrequency, 0.34], [baseFrequency * 1.5, 0.16]]) {
          const osc = ac.createOscillator()
          const gain = ac.createGain()
          osc.type = i % 2 === 0 ? 'square' : 'sawtooth'
          osc.frequency.setValueAtTime(frequency, pulseAt)
          gain.gain.setValueAtTime(0.0001, pulseAt)
          gain.gain.exponentialRampToValueAtTime(volume, pulseAt + 0.012)
          gain.gain.exponentialRampToValueAtTime(0.0001, pulseAt + pulseLength)
          osc.connect(gain); gain.connect(bus)
          osc.start(pulseAt); osc.stop(pulseAt + pulseLength + 0.02)
          sources.push(osc)
        }
      }
      let stopped = false
      return trackStop(() => {
        if (stopped) return
        stopped = true
        const now = ac.currentTime
        try {
          bus.gain.cancelScheduledValues(now)
          bus.gain.setTargetAtTime(0.0001, now, 0.015)
        } catch (e) {}
        for (const source of sources) { try { source.stop(now + 0.08) } catch (e) {} }
        window.setTimeout(() => { try { bus.disconnect() } catch (e) {} }, 120)
      })
    },
    sfxFirstBlood() {
      const ac = on()
      if (!ac) return () => {}
      const bus = ac.createGain()
      bus.gain.value = 1
      bus.connect(masterGain)
      const sources: any[] = []
      const remember = (source: any) => { if (source) sources.push(source) }
      remember(noiseBurst({ type: 'lowpass', f: 1800, fEnd: 180, dur: 0.55, vol: 0.28, output: bus }))
      remember(tone({ type: 'sawtooth', f: 150, f2: 42, dur: 0.62, vol: 0.28, output: bus }))
      ;[220, 330, 440].forEach((f, i) => remember(tone({
        type: 'square', f, f2: f * 0.98, dur: 0.82, vol: 0.1,
        delay: 0.16 + i * 0.025, output: bus,
      })))
      remember(noiseBurst({
        type: 'highpass', f: 4200, fEnd: 9200, dur: 0.3,
        vol: 0.1, delay: 0.12, output: bus,
      }))
      const stop = trackStop(() => {
        const now = ac.currentTime
        try {
          bus.gain.cancelScheduledValues(now)
          bus.gain.setTargetAtTime(0.0001, now, 0.015)
        } catch (e) {}
        for (const source of sources) { try { source.stop(now + 0.08) } catch (e) {} }
        window.setTimeout(() => { try { bus.disconnect() } catch (e) {} }, 120)
      })
      window.setTimeout(stop, 1200)
      return stop
    },
  }
}
