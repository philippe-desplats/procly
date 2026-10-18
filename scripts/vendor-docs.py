#!/usr/bin/env python3
"""Regenerate the vendored reference docs under docs/reference/.

Rust crates are rendered from the rustdoc JSON that docs.rs publishes for the
pinned version. Tauri v2 pages are fetched from the official tauri-docs
repository at a pinned commit, with the MDX syntax reduced to plain Markdown.

Bump a version or the commit below, run `python3 scripts/vendor-docs.py`, and
review the diff. Standard library only (needs Python 3.14+ for compression.zstd).
"""

from __future__ import annotations

import json
import re
import sys
import urllib.request
from compression import zstd
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "reference"
USER_AGENT = "procly-vendor-docs (+https://github.com/philippe-desplats/procly)"

# Crates named in ADR 0002.
CRATES = [
    ("process-wrap", "10.0.1", "Process groups, sessions, Job Objects and kill-on-drop wrappers over std and tokio Command"),
    ("portable-pty", "0.9.0", "Cross-platform pseudo terminals (Unix PTY and Windows ConPTY), from WezTerm"),
]

TAURI_DOCS_COMMIT = "b9dd7032c3a872cad5995bc78c2946f087019942"
TAURI_DOCS_RAW = f"https://raw.githubusercontent.com/tauri-apps/tauri-docs/{TAURI_DOCS_COMMIT}/src/content/docs"
TAURI_PAGES = [
    "concept/architecture.mdx",
    "concept/process-model.md",
    "concept/Inter-Process Communication/index.mdx",
    "concept/Inter-Process Communication/isolation.md",
    "start/prerequisites.mdx",
    "start/project-structure.mdx",
    "develop/configuration-files.mdx",
    "develop/calling-rust.mdx",
    "develop/calling-frontend.mdx",
    "develop/state-management.mdx",
    "develop/sidecar.mdx",
    "develop/resources.mdx",
    "develop/Plugins/index.mdx",
    "develop/Tests/index.mdx",
    "develop/Tests/mocking.mdx",
    "security/index.mdx",
    "security/capabilities.mdx",
    "security/permissions.mdx",
    "security/scope.mdx",
    "security/runtime-authority.mdx",
    "security/csp.mdx",
]


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


# ---------------------------------------------------------------------------
# rustdoc JSON to Markdown
# ---------------------------------------------------------------------------


class Rustdoc:
    def __init__(self, data: dict) -> None:
        self.index = data["index"]
        self.root = data["root"]

    def item(self, item_id) -> dict | None:
        return self.index.get(str(item_id))

    # -- types --------------------------------------------------------------

    def ty(self, t) -> str:
        if t is None:
            return "()"
        (kind, v), = t.items()
        if kind == "resolved_path":
            return self.path(v)
        if kind == "generic":
            return v
        if kind == "primitive":
            return v
        if kind == "borrowed_ref":
            lifetime = f"{v['lifetime']} " if v.get("lifetime") else ""
            mutable = "mut " if v["is_mutable"] else ""
            return f"&{lifetime}{mutable}{self.ty(v['type'])}"
        if kind == "raw_pointer":
            return f"*{'mut' if v['is_mutable'] else 'const'} {self.ty(v['type'])}"
        if kind == "slice":
            return f"[{self.ty(v)}]"
        if kind == "array":
            return f"[{self.ty(v['type'])}; {v['len']}]"
        if kind == "tuple":
            inner = ", ".join(self.ty(x) for x in v)
            return f"({inner},)" if len(v) == 1 else f"({inner})"
        if kind == "dyn_trait":
            parts = [self.poly_trait(x) for x in v["traits"]]
            if v.get("lifetime"):
                parts.append(v["lifetime"])
            return "dyn " + " + ".join(parts)
        if kind == "impl_trait":
            return "impl " + " + ".join(self.bound(b) for b in v)
        if kind == "qualified_path":
            trait = f" as {self.path(v['trait'])}" if v.get("trait") else ""
            return f"<{self.ty(v['self_type'])}{trait}>::{v['name']}"
        if kind == "function_pointer":
            inputs = ", ".join(self.ty(x[1]) for x in v["sig"]["inputs"])
            output = f" -> {self.ty(v['sig']['output'])}" if v["sig"]["output"] else ""
            return f"fn({inputs}){output}"
        if kind == "infer":
            return "_"
        if kind == "pat":
            return self.ty(v["type"])
        return f"<{kind}>"

    def path(self, p: dict) -> str:
        name = p["path"].split("::")[-1] if p["path"].startswith("::") else p["path"]
        return name + self.generic_args(p.get("args"))

    def generic_args(self, args) -> str:
        if not args:
            return ""
        if "angle_bracketed" in args:
            ab = args["angle_bracketed"]
            parts = []
            for a in ab["args"]:
                (k, v), = a.items() if isinstance(a, dict) else (("lifetime", a),)
                if k == "type":
                    parts.append(self.ty(v))
                elif k == "lifetime":
                    parts.append(v)
                elif k == "const":
                    parts.append(v.get("expr", "_"))
                else:
                    parts.append("_")
            for c in ab.get("constraints", []):
                binding = c.get("binding", {})
                if "equality" in binding:
                    eq = binding["equality"]
                    value = self.ty(eq["type"]) if "type" in eq else "_"
                    parts.append(f"{c['name']} = {value}")
                elif "constraint" in binding:
                    parts.append(f"{c['name']}: " + " + ".join(self.bound(b) for b in binding["constraint"]))
            return f"<{', '.join(parts)}>" if parts else ""
        if "parenthesized" in args:
            pa = args["parenthesized"]
            inputs = ", ".join(self.ty(x) for x in pa["inputs"])
            output = f" -> {self.ty(pa['output'])}" if pa.get("output") else ""
            return f"({inputs}){output}"
        return ""

    def poly_trait(self, pt: dict) -> str:
        return self.path(pt["trait"])

    def bound(self, b: dict) -> str:
        (kind, v), = b.items()
        if kind == "trait_bound":
            modifier = "?" if v.get("modifier") == "maybe" else ""
            return modifier + self.path(v["trait"])
        if kind == "outlives":
            return v
        if kind == "use":
            return "use<..>"
        return kind

    def generics(self, g: dict) -> tuple[str, str]:
        params = []
        for p in g["params"]:
            kind = p["kind"]
            if "lifetime" in kind:
                params.append(p["name"])
            elif "type" in kind:
                if kind["type"].get("is_synthetic"):
                    continue
                bounds = kind["type"]["bounds"]
                params.append(p["name"] + (": " + " + ".join(self.bound(b) for b in bounds) if bounds else ""))
            elif "const" in kind:
                params.append(f"const {p['name']}: {self.ty(kind['const']['type'])}")
        where = []
        for w in g.get("where_predicates", []):
            if "bound_predicate" in w:
                bp = w["bound_predicate"]
                where.append(f"{self.ty(bp['type'])}: " + " + ".join(self.bound(b) for b in bp["bounds"]))
        return (f"<{', '.join(params)}>" if params else "", f" where {', '.join(where)}" if where else "")

    def fn_sig(self, item: dict) -> str:
        f = item["inner"]["function"]
        header = f["header"]
        prefix = "".join(
            word for flag, word in (("is_const", "const "), ("is_async", "async "), ("is_unsafe", "unsafe ")) if header[flag]
        )
        params, where = self.generics(f["generics"])
        inputs = []
        for name, t in f["sig"]["inputs"]:
            if name == "self":
                rendered = self.ty(t)
                inputs.append({"Self": "self", "&Self": "&self", "&mut Self": "&mut self"}.get(rendered, f"self: {rendered}"))
            else:
                inputs.append(f"{name}: {self.ty(t)}")
        output = f" -> {self.ty(f['sig']['output'])}" if f["sig"]["output"] else ""
        return f"{prefix}fn {item['name']}{params}({', '.join(inputs)}){output}{where}"

    # -- items --------------------------------------------------------------

    def walk(self) -> list[tuple[str, dict]]:
        """Public items reachable from the crate root, with their module path."""
        found: list[tuple[str, dict]] = []
        seen: set = set()

        def visit(module_id, prefix: str) -> None:
            for child_id in self.item(module_id)["inner"]["module"]["items"]:
                child = self.item(child_id)
                if child is None or child_id in seen or child["visibility"] != "public":
                    continue
                seen.add(child_id)
                kind = next(iter(child["inner"]))
                if kind == "module":
                    found.append((f"{prefix}::{child['name']}", child))
                    visit(child_id, f"{prefix}::{child['name']}")
                elif kind == "use":
                    use = child["inner"]["use"]
                    target = self.item(use["id"]) if use.get("id") is not None else None
                    if target and next(iter(target["inner"])) == "module" and use["is_glob"]:
                        visit(use["id"], prefix)
                    elif target and use["id"] not in seen:
                        seen.add(use["id"])
                        found.append((prefix, {**target, "name": use["name"]}))
                else:
                    found.append((prefix, child))

        visit(self.root, self.item(self.root)["name"])
        return found

    def fields(self, field_ids) -> list[dict]:
        return [f for f in (self.item(i) for i in field_ids if i is not None) if f and f["visibility"] == "public"]

    def struct_decl(self, head: str, shape) -> list[str]:
        if shape == "unit":
            return [head + ";"]
        if "tuple" in shape:
            types = [f"pub {self.ty(f['inner']['struct_field'])}" for f in self.fields(shape["tuple"])]
            return [f"{head}({', '.join(types)});"]
        plain = shape["plain"]
        lines = [head + " {"]
        for field in self.fields(plain["fields"]):
            lines += [f"    /// {line}".rstrip() for line in (field["docs"] or "").strip().splitlines()]
            lines.append(f"    pub {field['name']}: {self.ty(field['inner']['struct_field'])},")
        if plain["has_stripped_fields"]:
            lines.append("    /* private fields */")
        return [*lines, "}"]

    def variant_decl(self, variant: dict) -> str:
        shape = variant["inner"]["variant"]["kind"]
        if shape == "plain":
            return variant["name"]
        if "tuple" in shape:
            types = [self.ty(f["inner"]["struct_field"]) for f in self.fields(shape["tuple"])]
            return f"{variant['name']}({', '.join(types)})"
        fields = [f"{f['name']}: {self.ty(f['inner']['struct_field'])}" for f in self.fields(shape["struct"]["fields"])]
        return f"{variant['name']} {{ {', '.join(fields)} }}"

    def methods(self, impl_ids) -> tuple[list[dict], list[str]]:
        inherent, traits = [], []
        for impl_id in impl_ids:
            impl = self.item(impl_id)
            if impl is None:
                continue
            inner = impl["inner"]["impl"]
            if inner.get("is_synthetic") or inner.get("blanket_impl"):
                continue
            if inner["trait"] is None:
                for m in inner["items"]:
                    method = self.item(m)
                    if method and method["visibility"] == "public" and "function" in method["inner"]:
                        inherent.append(method)
            else:
                traits.append(self.path(inner["trait"]))
        return inherent, sorted(set(traits))

    def render(self) -> str:
        lines = [doc_text(self.item(self.root)["docs"], 1), ""]
        for prefix, item in self.walk():
            kind = next(iter(item["inner"]))
            inner = item["inner"][kind]
            name = item["name"]
            if kind == "module":
                lines += [f"## Module `{prefix}`", "", doc_text(item["docs"], 2), ""]
                continue
            lines += [f"### {kind} `{prefix}::{name}`", ""]
            if kind == "function":
                lines += ["```rust", "pub " + self.fn_sig(item), "```", ""]
            elif kind == "struct":
                params, where = self.generics(inner["generics"])
                lines += ["```rust", *self.struct_decl(f"pub struct {name}{params}{where}", inner["kind"]), "```", ""]
            elif kind in ("enum", "union", "trait"):
                params, where = self.generics(inner["generics"])
                lines += ["```rust", f"pub {kind} {name}{params}{where}", "```", ""]
            elif kind == "type_alias":
                params, _ = self.generics(inner["generics"])
                lines += ["```rust", f"pub type {name}{params} = {self.ty(inner['type'])};", "```", ""]
            elif kind == "constant":
                lines += ["```rust", f"pub const {name}: {self.ty(inner['type'])};", "```", ""]
            lines += [doc_text(item["docs"], 4), ""]
            if kind == "enum":
                for variant_id in inner["variants"]:
                    variant = self.item(variant_id)
                    summary = (variant["docs"] or "").strip().split("\n", 1)[0]
                    lines.append(f"- `{self.variant_decl(variant)}`" + (f": {summary}" if summary else ""))
                lines.append("")
            if kind in ("struct", "enum"):
                inherent, traits = self.methods(inner["impls"])
                for method in inherent:
                    lines += [f"#### `{name}::{method['name']}`", "", "```rust", "pub " + self.fn_sig(method), "```", ""]
                    lines += [doc_text(method["docs"], 4), ""]
                if traits:
                    lines += ["Implements: " + ", ".join(f"`{t}`" for t in traits), ""]
            if kind == "trait":
                for member_id in inner["items"]:
                    member = self.item(member_id)
                    if member is None:
                        continue
                    if "function" in member["inner"]:
                        provided = " (provided)" if member["inner"]["function"]["has_body"] else ""
                        lines += [f"#### `{name}::{member['name']}`{provided}", "", "```rust", self.fn_sig(member), "```", ""]
                        lines += [doc_text(member["docs"], 4), ""]
                    elif "assoc_type" in member["inner"]:
                        lines += [f"#### `{name}::{member['name']}` (associated type)", ""]
                implementors = [self.item(i) for i in inner.get("implementations", [])]
                names = sorted({self.ty(i["inner"]["impl"]["for"]) for i in implementors if i})
                if names:
                    lines += ["Implemented by: " + ", ".join(f"`{n}`" for n in names), ""]
        return re.sub(r"\n{3,}", "\n\n", "\n".join(lines))


RUST_FENCES = ("rust", "no_run", "ignore", "should_panic", "compile_fail")


def doc_text(docs: str | None, depth: int) -> str:
    """Shift doc headings below `depth` and drop the lines rustdoc hides in examples."""
    out, fence = [], None
    for line in (docs or "").strip().splitlines():
        stripped = line.lstrip()
        if stripped.startswith("```"):
            if fence is None:
                lang = stripped[3:].split(",")[0].strip()
                fence = lang or "rust"
                out.append("```" + ("rust" if fence in RUST_FENCES else lang))
            else:
                fence = None
                out.append("```")
            continue
        if fence in RUST_FENCES:
            if stripped == "#" or stripped.startswith("# "):
                continue
        elif fence is None and re.match(r"^#{1,6} ", line):
            line = "#" * depth + line
        out.append(line)
    return "\n".join(out)


def vendor_crate(crate: str, version: str, summary: str) -> str:
    raw = zstd.decompress(fetch(f"https://docs.rs/crate/{crate}/{version}/json"))
    markdown = Rustdoc(json.loads(raw)).render()
    path = OUT / "rust" / f"{crate}.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    header = (
        f"# {crate} {version}\n\n"
        f"> Vendored reference, generated by `scripts/vendor-docs.py` from the rustdoc JSON at\n"
        f"> <https://docs.rs/crate/{crate}/{version}/json>. Browse it online at <https://docs.rs/{crate}/{version}>.\n"
        f"> Do not edit by hand, regenerate instead.\n\n"
    )
    path.write_text(header + markdown.strip() + "\n")
    return f"| [{crate} {version}](rust/{crate}.md) | {summary} |"


# ---------------------------------------------------------------------------
# Tauri MDX to Markdown
# ---------------------------------------------------------------------------

IMPORT_SECTION = re.compile(r"^import \{ Content as (\w+) \} from '([^']+)';$")
COMMAND_TABS = re.compile(r"<CommandTabs([^>]*)/>", re.S)
TAB_ITEM = re.compile(r"^\s*<TabItem label=\"([^\"]+)\"[^>]*>\s*$")
JSX_LINE = re.compile(r"^\s*</?[A-Z][\w.]*(\s[^>]*)?/?>\s*$")
JSX_OPEN = re.compile(r"^\s*<[A-Z][\w.]*(\s[^>]*)?$")


def mdx_to_markdown(source: str, page: str) -> str:
    frontmatter = re.match(r"^---\n(.*?)\n---\n", source, re.S)
    title = page
    if frontmatter:
        match = re.search(r"^title:\s*(.+)$", frontmatter.group(1), re.M)
        title = match.group(1).strip().strip("'\"") if match else page
        source = source[frontmatter.end():]

    def command_tabs(match: re.Match) -> str:
        attrs = dict(re.findall(r'(\w+)="([^"]*)"', match.group(1)))
        commands = [f"{attrs[k]}" for k in ("cargo", "npm") if k in attrs]
        return "```sh\n" + "\n".join(commands) + "\n```" if commands else ""

    source = COMMAND_TABS.sub(command_tabs, source)
    sections: dict[str, str] = {}
    out, in_fence, in_tag = [], False, False
    for line in source.splitlines():
        if in_tag:
            in_tag = not line.rstrip().endswith(">")
            continue
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            out.append(line)
            continue
        if in_fence:
            out.append(line)
            continue
        section = IMPORT_SECTION.match(line)
        if section:
            base = page.rsplit("/", 1)[0]
            sections[section.group(1)] = f"{base}/{section.group(2).removeprefix('./')}"
            continue
        if line.startswith("import ") or line.startswith("export "):
            continue
        tag = re.match(r"^\s*<(\w+)\s*/>\s*$", line)
        if tag and tag.group(1) in sections:
            included = fetch(f"{TAURI_DOCS_RAW}/{sections[tag.group(1)]}").decode()
            out.append(mdx_to_markdown(included, sections[tag.group(1)]).split("\n", 2)[2])
            continue
        tab = TAB_ITEM.match(line)
        if tab:
            out.append(f"**{tab.group(1)}:**")
            continue
        if JSX_LINE.match(line) or line.strip().startswith("{/*"):
            continue
        if JSX_OPEN.match(line):
            in_tag = True
            continue
        out.append(line)
    body = "\n".join(out)
    # Site-relative links point at the published docs.
    body = re.sub(r"\]\((/[^)]*)\)", r"](https://v2.tauri.app\1)", body)
    return f"# {title}\n\n" + re.sub(r"\n{3,}", "\n\n", body).strip() + "\n"


def vendor_tauri_page(page: str) -> str:
    source = fetch(f"{TAURI_DOCS_RAW}/{page.replace(' ', '%20')}").decode()
    markdown = mdx_to_markdown(source, page)
    route = re.sub(r"(/index)?\.mdx?$", "", page).lower().replace(" ", "-")
    slug = route.replace("/", "--")
    path = OUT / "tauri-v2" / f"{slug}.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://v2.tauri.app/{route}/"
    title, rest = markdown.split("\n", 1)
    header = (
        f"\n> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/{TAURI_DOCS_COMMIT}/src/content/docs/"
        f"{page.replace(' ', '%20')}>\n> (published at <{url}>) by `scripts/vendor-docs.py`, MIT licensed.\n"
        f"> Do not edit by hand, regenerate instead.\n"
    )
    path.write_text(title + "\n" + header + rest)
    return f"| [{title.removeprefix('# ')}](tauri-v2/{slug}.md) | {url} |"


def main() -> int:
    crate_rows = [vendor_crate(c, v, s) for c, v, s in CRATES]
    tauri_rows = [vendor_tauri_page(p) for p in TAURI_PAGES]
    index = "\n".join(
        [
            "# Reference docs",
            "",
            "Official documentation vendored into the repository so that agents read the exact API of the pinned",
            "versions instead of recalling an older one. Everything here is generated by `scripts/vendor-docs.py`,",
            "never edited by hand. Bump a version or the Tauri docs commit in the script, rerun it, review the diff.",
            "",
            "Project rules that interpret these docs live in [`docs/rules/`](../rules/).",
            "",
            "## Rust crates (ADR 0002)",
            "",
            "Rendered from the docs.rs build for Linux, so Windows-only items (`process-wrap` Job Objects, `portable-pty`",
            "ConPTY) are documented on docs.rs only.",
            "",
            "| Crate | Use in Procly |",
            "| --- | --- |",
            *crate_rows,
            "",
            f"## Tauri v2 (tauri-docs at `{TAURI_DOCS_COMMIT[:12]}`)",
            "",
            "| Page | Published at |",
            "| --- | --- |",
            *tauri_rows,
            "",
        ]
    )
    (OUT / "README.md").write_text(index)
    print(f"vendored {len(crate_rows)} crates and {len(tauri_rows)} Tauri pages into {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
