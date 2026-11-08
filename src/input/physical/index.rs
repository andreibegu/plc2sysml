use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct Index {
    pub files: BTreeMap<String, String>,
    definitions: BTreeMap<String, Definition>,
    transfers: BTreeMap<String, Vec<(String, String)>>,
}

#[derive(Debug, Clone, Default)]
struct Definition {
    kind: String,
    base: Option<String>,
    members: BTreeMap<String, Member>,
}

#[derive(Debug, Clone)]
struct Member {
    kind: String,
    direction: String,
    declared_in: String,
    ty: Option<String>,
    inline: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SignalParameter {
    pub physical: String,
    pub features: Vec<String>,
    pub item_type: Option<String>,
    pub direction: String,
}

impl Index {
    pub fn load(folder: &Path) -> Result<Self> {
        if !folder.is_dir() {
            bail!("physical_model must be a folder of SysML files");
        }

        let mut index = Self::default();
        let mut paths = vec![];

        for entry in WalkDir::new(folder).follow_links(false).sort_by_file_name() {
            let entry = entry?;

            if entry.file_type().is_symlink() {
                bail!(
                    "Physical model contains a symbolic link: {}",
                    entry.path().display()
                );
            }

            if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "sysml")
            {
                paths.push(entry.path().to_path_buf());
            }
        }

        if paths.is_empty() {
            bail!("Physical model needs at least one SysML file");
        }

        for path in paths {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("Cannot read physical model {}", path.display()))?;
            let relative = path
                .strip_prefix(folder)?
                .to_str()
                .context("Physical model filenames must be UTF-8")?;
            let tokens = lex(&text)?;
            let mut cursor = 0;
            index.scope(&tokens, &mut cursor, "", 0)?;
            index.files.insert(format!("physical/{relative}"), text);
        }

        Ok(index)
    }

    pub fn root_definition(&self) -> Result<String> {
        let mut referenced = BTreeSet::new();

        for (scope, def) in &self.definitions {
            if let Some(base) = &def.base
                && let Ok(ty) = self.resolve_type(base, scope)
            {
                referenced.insert(ty);
            }

            for member in def.members.values().filter(|m| m.kind == "part") {
                if let Some(ty) = &member.ty
                    && let Ok(ty) = self.resolve_type(ty, &member.declared_in)
                {
                    referenced.insert(ty);
                }
            }
        }

        let roots: Vec<_> = self
            .definitions
            .iter()
            .filter(|(key, def)| def.kind == "part" && !referenced.contains(*key))
            .map(|(key, _)| key.clone())
            .collect();

        if let [root] = roots.as_slice() {
            Ok(root.clone())
        } else {
            bail!(
                "Structural model has {} possible roots; provide root_definition explicitly",
                roots.len()
            );
        }
    }

    pub fn require_definition(&self, qualified: &str, kind: &str) -> Result<()> {
        let key = self.resolve_type(qualified, "")?;

        if self.definitions[&key].kind != kind {
            bail!("{qualified} must be a {kind} definition");
        }

        Ok(())
    }

    pub fn type_at_path(&self, root: &str, path: &str) -> Result<String> {
        let mut scope = self.resolve_type(root, "")?;

        if path.is_empty() {
            return Ok(scope);
        }

        for local in path.split('.') {
            let member = self
                .member(&scope, local, &mut BTreeSet::new())
                .context("Physical member not found")?;
            scope = if let Some(inline) = &member.inline {
                inline.clone()
            } else {
                let ty = member.ty.as_ref().context("Physical member is untyped")?;
                self.resolve_type(ty, &member.declared_in)?
            };
        }

        Ok(scope)
    }

    pub fn resolve_path(&self, root: &str, path: &str) -> Result<()> {
        let mut scope = self.resolve_type(root, "")?;

        if path.is_empty() {
            bail!("Physical association must identify a part usage inside the root definition");
        }

        let mut names = path.split('.').peekable();

        while let Some(name) = names.next() {
            if name.is_empty() {
                bail!("Empty physical path segment in {path}");
            }

            let member = self
                .member(&scope, name, &mut BTreeSet::new())
                .with_context(|| format!("Unknown physical part {path} (at {name})"))?;

            if member.kind != "part" {
                bail!("Physical path {path} must contain only parts");
            }

            if names.peek().is_some() {
                scope = if let Some(inline) = &member.inline {
                    inline.clone()
                } else {
                    let ty = member
                        .ty
                        .as_ref()
                        .with_context(|| format!("Untyped physical part {path}"))?;
                    self.resolve_type(ty, &member.declared_in)?
                };
            }
        }

        Ok(())
    }

    pub fn physical_transfers(&self, root: &str) -> Result<BTreeSet<(String, String)>> {
        let mut pairs = BTreeSet::new();
        let mut pending = vec![(
            self.resolve_type(root, "")?,
            String::new(),
            Vec::<String>::new(),
        )];

        while let Some((scope, prefix, mut ancestors)) = pending.pop() {
            if ancestors.len() >= 128 || ancestors.contains(&scope) {
                bail!("Cyclic or excessive physical containment at {prefix}");
            }

            ancestors.push(scope.clone());

            let mut members = BTreeMap::new();
            let mut current = Some(scope.clone());
            let mut bases = BTreeSet::new();

            while let Some(ty) = current {
                if !bases.insert(ty.clone()) {
                    bail!("Cyclic physical inheritance at {ty}");
                }

                if let Some(flows) = self.transfers.get(&ty) {
                    for (from, to) in flows {
                        let qualify = |p: &str| {
                            if prefix.is_empty() {
                                p.into()
                            } else {
                                format!("{prefix}.{p}")
                            }
                        };
                        let from = qualify(from);
                        let to = qualify(to);

                        if let (Some(from), Some(to)) =
                            (self.part_owner(root, &from), self.part_owner(root, &to))
                        {
                            pairs.insert((from, to));
                        }
                    }
                }

                let def = &self.definitions[&ty];

                for (name, member) in &def.members {
                    members.entry(name.clone()).or_insert(member);
                }

                current = def
                    .base
                    .as_ref()
                    .map(|b| self.resolve_type(b, &ty))
                    .transpose()?;
            }

            for (name, member) in members.into_iter().filter(|(_, m)| m.kind == "part") {
                let ty = if let Some(inline) = &member.inline {
                    inline.clone()
                } else {
                    self.resolve_type(
                        member.ty.as_ref().context("Untyped physical part")?,
                        &member.declared_in,
                    )?
                };

                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}.{name}")
                };

                pending.push((ty, path, ancestors.clone()));
            }
        }

        Ok(pairs)
    }

    fn part_owner(&self, root: &str, endpoint: &str) -> Option<String> {
        let mut path = endpoint;

        loop {
            if self.resolve_path(root, path).is_ok() {
                return Some(path.into());
            }

            path = path.rsplit_once('.')?.0;
        }
    }

    pub fn signal_direction(&self, root: &str, target: &str) -> Result<Option<String>> {
        if self.resolve_path(root, target).is_ok() {
            let ty = self.type_at_path(root, target)?;
            let mut members = BTreeMap::new();
            self.item_members(&ty, &mut BTreeSet::new(), &mut members)?;

            let directions: BTreeSet<_> = members.values().map(|m| m.direction.as_str()).collect();
            return Ok(if directions.len() == 1 {
                directions
                    .iter()
                    .next()
                    .filter(|d| matches!(**d, "in" | "out"))
                    .map(|d| d.to_string())
            } else {
                None
            });
        }

        let endpoint = self.signal_endpoint(root, target, "in")?;

        Ok(matches!(endpoint.direction.as_str(), "in" | "out").then_some(endpoint.direction))
    }

    pub fn signal_endpoint(
        &self,
        root: &str,
        target: &str,
        direction: &str,
    ) -> Result<SignalParameter> {
        if self.resolve_path(root, target).is_ok() {
            let ty = self.type_at_path(root, target)?;
            let mut candidates = BTreeMap::new();
            self.item_members(&ty, &mut BTreeSet::new(), &mut candidates)?;

            let candidates: Vec<_> = candidates
                .into_iter()
                .filter(|(_, m)| m.direction == direction)
                .collect();

            if candidates.is_empty() {
                return Ok(SignalParameter {
                    physical: target.into(),
                    features: vec![],
                    item_type: None,
                    direction: direction.into(),
                });
            }

            let [(name, member)] = candidates.as_slice() else {
                bail!(
                    "{target}: {} {direction} item endpoints; specify an explicit part.item or part.action.parameter path",
                    candidates.len()
                );
            };

            return self.item_endpoint(target, vec![name.clone()], member);
        }

        let (physical, feature) = target
            .rsplit_once('.')
            .context("Target must identify a physical part or signal endpoint")?;

        if self.resolve_path(root, physical).is_ok() {
            let ty = self.type_at_path(root, physical)?;
            let member = self
                .member(&ty, feature, &mut BTreeSet::new())
                .with_context(|| format!("Unknown signal endpoint {target}"))?;
            return self.item_endpoint(physical, vec![feature.into()], member);
        }

        let (physical, action) = physical
            .rsplit_once('.')
            .context("Target must identify a physical part or signal endpoint")?;

        self.resolve_path(root, physical)?;

        self.function_parameter(root, physical, action, feature)
    }

    fn item_members<'a>(
        &'a self,
        scope: &str,
        seen: &mut BTreeSet<String>,
        out: &mut BTreeMap<String, &'a Member>,
    ) -> Result<()> {
        if !seen.insert(scope.into()) {
            bail!("Cyclic physical type inheritance at {scope}");
        }

        let def = &self.definitions[scope];

        if let Some(base) = &def.base {
            self.item_members(&self.resolve_type(base, scope)?, seen, out)?;
        }

        for (name, member) in &def.members {
            out.remove(name);

            if member.kind == "item" {
                out.insert(name.clone(), member);
            }
        }

        Ok(())
    }

    fn item_endpoint(
        &self,
        physical: &str,
        features: Vec<String>,
        member: &Member,
    ) -> Result<SignalParameter> {
        if member.kind != "item" {
            bail!(
                "{physical}.{} must identify an item endpoint",
                features.join(".")
            );
        }

        Ok(SignalParameter {
            physical: physical.into(),
            features,
            item_type: Some(self.resolve_type(
                member.ty.as_ref().context("Untyped signal endpoint")?,
                &member.declared_in,
            )?),
            direction: member.direction.clone(),
        })
    }

    fn function_parameter(
        &self,
        root: &str,
        part: &str,
        action: &str,
        parameter: &str,
    ) -> Result<SignalParameter> {
        let ty = self.type_at_path(root, part)?;
        let usage = self
            .member(&ty, action, &mut BTreeSet::new())
            .with_context(|| format!("Unknown performed action {part}.{action}"))?;

        if usage.kind != "action" {
            bail!("{part}.{action} must be a performed action");
        }

        let definition = self.resolve_type(
            usage.ty.as_ref().context("Untyped action")?,
            &usage.declared_in,
        )?;
        let param = self
            .member(&definition, parameter, &mut BTreeSet::new())
            .with_context(|| format!("Unknown action parameter {action}.{parameter}"))?;

        if param.kind != "item" {
            bail!("Function binding must identify an item parameter");
        }

        Ok(SignalParameter {
            physical: part.into(),
            features: vec![action.into(), parameter.into()],
            item_type: Some(self.resolve_type(
                param.ty.as_ref().context("Untyped function parameter")?,
                &param.declared_in,
            )?),
            direction: param.direction.clone(),
        })
    }

    fn member<'a>(
        &'a self,
        scope: &str,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> Option<&'a Member> {
        if !seen.insert(scope.to_string()) {
            return None;
        }

        let def = self.definitions.get(scope)?;

        if let Some(member) = def.members.get(name) {
            return Some(member);
        }

        self.member(
            &self.resolve_type(def.base.as_ref()?, scope).ok()?,
            name,
            seen,
        )
    }

    fn resolve_type(&self, ty: &str, scope: &str) -> Result<String> {
        if self.definitions.contains_key(ty) {
            return Ok(ty.to_string());
        }

        let mut containing = scope;

        while let Some((parent, _)) = containing.rsplit_once("::") {
            let qualified = format!("{parent}::{ty}");

            if self.definitions.contains_key(&qualified) {
                return Ok(qualified);
            }

            containing = parent;
        }

        let candidates: Vec<_> = self
            .definitions
            .iter()
            .filter(|(key, def)| {
                !def.kind.ends_with("usage") && key.rsplit("::").next() == Some(ty)
            })
            .map(|(key, _)| key)
            .collect();

        if candidates.len() != 1 {
            bail!("Physical type {ty} is missing or ambiguous; use a qualified name");
        }

        Ok(candidates[0].clone())
    }

    fn scope(
        &mut self,
        tokens: &[String],
        cursor: &mut usize,
        scope: &str,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 {
            bail!("Physical model index depth exceeded");
        }

        while *cursor < tokens.len() {
            if tokens[*cursor] == "}" {
                *cursor += 1;
                return Ok(());
            }

            let keyword = tokens[*cursor].as_str();

            if keyword == "flow" {
                let start = *cursor;

                while *cursor < tokens.len() && tokens[*cursor] != ";" && tokens[*cursor] != "}" {
                    *cursor += 1;
                }

                let header = &tokens[start..*cursor];

                if let (Some(from), Some(to)) = (
                    header.iter().position(|t| t == "from"),
                    header.iter().position(|t| t == "to"),
                ) && from + 1 < to
                    && to + 1 < header.len()
                {
                    self.transfers
                        .entry(scope.into())
                        .or_default()
                        .push((header[from + 1..to].join(""), header[to + 1..].join("")));
                }

                if tokens.get(*cursor).is_some_and(|t| t == ";") {
                    *cursor += 1;
                }

                continue;
            }

            let is_package = keyword == "package";
            let definition = matches!(keyword, "part" | "item" | "action" | "attribute" | "port")
                && tokens.get(*cursor + 1).is_some_and(|s| s == "def");
            let usage =
                matches!(keyword, "part" | "action" | "item" | "attribute" | "port") && !definition;

            if is_package || definition || usage {
                let kind = keyword.to_string();
                *cursor += if definition { 2 } else { 1 };

                let Some(local) = tokens
                    .get(*cursor)
                    .filter(|s| !matches!(s.as_str(), ":" | "{" | ";"))
                else {
                    continue;
                };

                let local = local.clone();
                *cursor += 1;

                let qualified = if scope.is_empty() {
                    local.clone()
                } else {
                    format!("{scope}::{local}")
                };

                let mut header = vec![];

                while *cursor < tokens.len() && !matches!(tokens[*cursor].as_str(), "{" | ";" | "}")
                {
                    header.push(tokens[*cursor].clone());
                    *cursor += 1;
                }

                let ty = header
                    .iter()
                    .position(|s| s == if usage { ":" } else { ":>" })
                    .and_then(|i| qualified_name(&header[i + 1..]));
                let body = tokens.get(*cursor).is_some_and(|s| s == "{");

                if usage {
                    let member = Member {
                        kind: kind.clone(),
                        declared_in: scope.to_string(),
                        direction: cursor
                            .checked_sub(header.len() + 3)
                            .and_then(|i| tokens.get(i))
                            .filter(|s| matches!(s.as_str(), "in" | "out" | "inout"))
                            .cloned()
                            .unwrap_or_default(),
                        ty: ty.clone(),
                        inline: body.then(|| qualified.clone()),
                    };

                    let containing = self.definitions.entry(scope.to_string()).or_default();

                    if containing.members.insert(local.clone(), member).is_some() {
                        bail!("Duplicate indexed physical member {qualified}");
                    }
                }

                if definition || usage && body {
                    let def = Definition {
                        kind: if usage { format!("{kind} usage") } else { kind },
                        base: ty,
                        members: BTreeMap::new(),
                    };

                    if self.definitions.insert(qualified.clone(), def).is_some() {
                        bail!("Duplicate indexed physical definition {qualified}");
                    }
                }

                if body {
                    *cursor += 1;
                    self.scope(tokens, cursor, &qualified, depth + 1)?;
                } else if tokens.get(*cursor).is_some_and(|s| s == ";") {
                    *cursor += 1;
                }
            } else if keyword == "{" {
                *cursor += 1;

                let mut braces = 1;

                while *cursor < tokens.len() && braces > 0 {
                    match tokens[*cursor].as_str() {
                        "{" => braces += 1,
                        "}" => braces -= 1,
                        _ => {}
                    }

                    *cursor += 1;
                }

                if braces != 0 {
                    bail!("Unbalanced physical-model body");
                }
            } else {
                *cursor += 1;
            }
        }

        if depth != 0 {
            bail!("Unclosed physical-model scope");
        }

        Ok(())
    }
}

fn qualified_name(tokens: &[String]) -> Option<String> {
    let first = tokens.first()?.clone();
    let mut name = first;
    let mut i = 1;

    while tokens.get(i).is_some_and(|s| s == "::") {
        name.push_str("::");
        name.push_str(tokens.get(i + 1)?);
        i += 2;
    }

    Some(name)
}

fn lex(text: &str) -> Result<Vec<String>> {
    let chars: Vec<_> = text.chars().collect();
    let mut result = vec![];
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }

            continue;
        }

        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;

            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }

            if i + 1 >= chars.len() {
                bail!("Unclosed physical-model comment");
            }

            i += 2;
            continue;
        }

        if c == '\'' || c == '"' {
            let delimiter = c;
            let mut value = String::new();
            i += 1;

            while i < chars.len() && chars[i] != delimiter {
                if chars[i] == '\\' {
                    i += 1;
                }

                if i < chars.len() {
                    value.push(chars[i]);
                    i += 1;
                }
            }

            if i == chars.len() {
                bail!("Unclosed physical-model quoted token");
            }

            i += 1;
            result.push(if delimiter == '\'' {
                value
            } else {
                "<string literal>".into()
            });
            continue;
        }

        if c.is_alphanumeric() || c == '_' {
            let start = i;
            i += 1;

            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }

            result.push(chars[start..i].iter().collect());
            continue;
        }

        if c == ':' && chars.get(i + 1) == Some(&':') {
            result.push("::".into());
            i += 2;
            continue;
        }

        if c == ':' && chars.get(i + 1) == Some(&'>') {
            result.push(":>".into());
            i += 2;

            if chars.get(i) == Some(&'>') {
                i += 1;
            }

            continue;
        }

        result.push(c.to_string());
        i += 1;
    }

    Ok(result)
}
