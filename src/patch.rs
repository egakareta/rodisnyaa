//! Workarounds.

/// **Only use this if you are seeing `TypeError: Cannot read properties of undefined (reading
/// 'decode')` in the AudioWorklet backend.**
///
/// Installs a `TextDecoder`/`TextEncoder` polyfill into every `AudioWorklet` before CPAL loads
/// its worklet module.
///
/// `AudioWorkletGlobalScope` intentionally does not expose `TextDecoder`/`TextEncoder` in
/// Chrome, so the wasm-bindgen glue imported by CPAL's audioworklet backend initializes its
/// cached decoder to `undefined` inside the worklet realm. Any string crossing the WASM/JS
/// boundary on the audio rendering thread throws, which escapes the worklet `process()` call
/// and permanently silences the node.
///
/// The polyfill must be evaluated in the worklet global before the wasm-bindgen module is
/// imported there, because ES imports are evaluated first. Worklet globals persist per
/// `AudioContext`, so this wraps `AudioWorklet.prototype.addModule` once: every later
/// `addModule` (including CPAL's internally created `AudioContext`, which euphorium does not
/// control) first loads a tiny module that defines the globals, then loads the requested
/// module. The wrapper composes with other `addModule` wrappers and is a no-op where
/// `AudioWorklet` is unavailable.
#[cfg(target_arch = "wasm32")]
pub fn ensure_audioworklet_text_polyfill() {
    const PATCH: &str = r##"(function() {
try {
if (globalThis.__euphoriumAudioWorkletPatched) return;
var proto = globalThis.AudioWorklet && globalThis.AudioWorklet.prototype;
if (!proto || typeof proto.addModule !== "function") return;
var origAddModule = proto.addModule;
var polyfillCode = `if(typeof globalThis.TextDecoder==='undefined'){globalThis.TextDecoder=class{constructor(l,o){this._f=!!(o&&o.fatal);this._b=!!(o&&o.ignoreBOM);}get encoding(){return'utf-8';}get fatal(){return this._f;}get ignoreBOM(){return this._b;}decode(i){if(i===undefined||i===null)return'';var b;if(i instanceof Uint8Array)b=i;else if(i instanceof ArrayBuffer)b=new Uint8Array(i);else if(i&&typeof i.length==='number'){try{b=new Uint8Array(i);}catch(e){return'';}}else return'';var n=b.length,p=0,o=[],f=this._f;if(n>=3&&b[0]===239&&b[1]===187&&b[2]===191){if(this._b)p=3;else{o.push(65279);p=3;}}function bad(){if(f)throw new TypeError('The encoded data was not valid for encoding utf-8');o.push(65533);}while(p<n){var x=b[p],c=-1,d=0;if(x<=127){o.push(x);p++;continue;}else if(x>=194&&x<=223){d=1;c=x&31;}else if(x>=224&&x<=239){d=2;c=x&15;}else if(x>=240&&x<=244){d=3;c=x&7;}else{bad();p++;continue;}if(p+d>=n){bad();p++;continue;}var k=true;for(var j=1;j<=d;j++){var y=b[p+j];if(y<128||y>191){k=false;break;}c=(c<<6)|(y&63);}if(!k){bad();p++;continue;}if((d===1&&c<128)||(d===2&&c<2048)||(d===3&&c<65536)||(c>=55296&&c<=57343)||c>1114111){bad();p++;continue;}if(c<=65535)o.push(c);else{c-=65536;o.push(55296+(c>>10),56320+(c&1023));}p+=d+1;}var r='',S=8192;for(var q=0;q<o.length;q+=S){r+=String.fromCharCode.apply(null,o.slice(q,q+S));}return r;}};}if(typeof globalThis.TextEncoder==='undefined'){globalThis.TextEncoder=class{get encoding(){return'utf-8';}encode(s){var t=s===undefined||s===null?'':String(s),m=t.length,q=[];for(var i=0;i<m;i++){var u=t.charCodeAt(i);if(u<128)q.push(u);else if(u<2048)q.push(192|(u>>6),128|(u&63));else if(u>=55296&&u<=56319&&i+1<m){var v=t.charCodeAt(i+1);if(v>=56320&&v<=57343){var w=65536+((u-55296)<<10)+(v-56320);q.push(240|(w>>18),128|((w>>12)&63),128|((w>>6)&63),128|(w&63));i++;}else q.push(239,191,189);}else if(u>=56320&&u<=57343)q.push(239,191,189);else q.push(224|(u>>12),128|((u>>6)&63),128|(u&63));}return new Uint8Array(q);}encodeInto(s,d){var e=this.encode(s),w=e.length<d.length?e.length:d.length;d.set(e.subarray(0,w));return{read:String(s).length,written:w};}};}`;
var polyfillUrl = null;
function getPolyfillUrl() {
if (!polyfillUrl) polyfillUrl = URL.createObjectURL(new Blob([polyfillCode], { type: "text/javascript" }));
return polyfillUrl;
}
proto.addModule = function(moduleURL) {
var self = this;
var rest = Array.prototype.slice.call(arguments, 1);
try { if (polyfillUrl !== null && String(moduleURL) === polyfillUrl) return origAddModule.apply(self, arguments); } catch (e) {}
var polyUrl;
try { polyUrl = getPolyfillUrl(); } catch (e) { return origAddModule.apply(self, arguments); }
function loadOriginal() { return origAddModule.apply(self, [moduleURL].concat(rest)); }
try {
return origAddModule.call(self, polyUrl).then(loadOriginal, loadOriginal);
} catch (e) {
return loadOriginal();
}
};
globalThis.__euphoriumAudioWorkletPatched = true;
} catch (e) {}
})();"##;

    let _ = js_sys::eval(PATCH);
}
