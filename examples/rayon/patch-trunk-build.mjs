import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const distPath = path.resolve(
    process.env.TRUNK_STAGING_DIR ?? process.env.TRUNK_DIST_DIR ?? "dist",
);
const htmlPath = path.join(
    distPath,
    path.basename(process.env.TRUNK_HTML_FILE ?? "index.html"),
);
const snippetsPath = path.join(distPath, "snippets");

if (!fs.existsSync(snippetsPath)) {
    throw new Error(
        `Trunk rayon snippets directory is missing: ${snippetsPath}`,
    );
}

const wasmBindgenJsPath = fs
    .readdirSync(distPath, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".js"))
    .map((entry) => path.join(distPath, entry.name))
    .find((filePath) => {
        const source = fs.readFileSync(filePath, "utf8");
        return source.includes("wbg_rayon_PoolBuilder");
    });

if (!wasmBindgenJsPath) {
    throw new Error("could not find Trunk's wasm-bindgen JavaScript module");
}

const wasmBindgenJs = fs.readFileSync(wasmBindgenJsPath, "utf8");
const staticImport = wasmBindgenJs.match(
    /import\s+\{\s*startWorkers\s*\}\s+from\s+(['"])([^'"]+workerHelpers(?:\.no-bundler)?\.js)\1;\s*/,
);

if (!staticImport) {
    throw new Error("could not find wasm-bindgen-rayon's worker helper import");
}

const workerHelperImportPath = staticImport[2];
const lazyImport = `import('${workerHelperImportPath}').then(({ startWorkers }) => startWorkers(arg0, arg1, wbg_rayon_PoolBuilder.__wrap(arg2)))`;
const startWorkersCall =
    "const ret = startWorkers(arg0, arg1, wbg_rayon_PoolBuilder.__wrap(arg2));";

if (!wasmBindgenJs.includes(startWorkersCall)) {
    throw new Error("could not find wasm-bindgen-rayon's startWorkers call");
}

const patchedWasmBindgenJs = wasmBindgenJs
    .replace(staticImport[0], "")
    .replace(startWorkersCall, `const ret = ${lazyImport};`);
const changedFiles = [];

if (patchedWasmBindgenJs !== wasmBindgenJs) {
    fs.writeFileSync(wasmBindgenJsPath, patchedWasmBindgenJs);
    changedFiles.push(wasmBindgenJsPath);
}

const mainModuleName = path.basename(wasmBindgenJsPath);
for (const snippetDir of fs.readdirSync(snippetsPath)) {
    if (!snippetDir.startsWith("wasm-bindgen-rayon-")) {
        continue;
    }

    const workerHelperPath = path.join(
        snippetsPath,
        snippetDir,
        "src",
        "workerHelpers.js",
    );
    if (!fs.existsSync(workerHelperPath)) {
        continue;
    }

    const workerHelper = fs.readFileSync(workerHelperPath, "utf8");
    const patchedWorkerHelper = workerHelper.replace(
        "await import('../../..')",
        `await import('../../../${mainModuleName}')`,
    );

    if (patchedWorkerHelper !== workerHelper) {
        fs.writeFileSync(workerHelperPath, patchedWorkerHelper);
        changedFiles.push(workerHelperPath);
    }
}

if (changedFiles.length === 0) {
    throw new Error("Trunk output was not changed by the rayon patch");
}

if (!fs.existsSync(htmlPath)) {
    throw new Error(`Trunk HTML output is missing: ${htmlPath}`);
}

let html = fs.readFileSync(htmlPath, "utf8");
for (const changedFile of changedFiles) {
    const assetPath = path
        .relative(distPath, changedFile)
        .split(path.sep)
        .join("/");
    const integrity = `sha384-${crypto
        .createHash("sha384")
        .update(fs.readFileSync(changedFile))
        .digest("base64")}`;

    html = html.replace(/<(?:link|script)\b[^>]*>/g, (tag) => {
        const assetAttribute = tag.match(/\b(?:href|src)="([^"]+)"/);
        if (!assetAttribute) {
            return tag;
        }

        const referencedPath = assetAttribute[1]
            .split(/[?#]/, 1)[0]
            .replace(/^\/+/, "");
        if (referencedPath !== assetPath || !tag.includes("integrity=")) {
            return tag;
        }

        return tag.replace(/integrity="[^"]*"/, `integrity="${integrity}"`);
    });
}

fs.writeFileSync(htmlPath, html);
