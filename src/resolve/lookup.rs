use super::*;

pub(crate) struct Lookup<'a> {
    model: &'a Program,
    index: Index,
}

impl<'a> Lookup<'a> {
    pub(crate) fn new(model: &Program) -> Lookup<'_> {
        Lookup {
            model,
            index: Index::new(model),
        }
    }

    pub(crate) fn endpoint(&self, unit: &Unit, expression: &Expr) -> Option<StorageRef> {
        if has_index(expression) {
            return None;
        }

        let text = reference_name(expression)?;
        let parts: Vec<_> = text.split('.').collect();

        if matches!(unit.kind, UnitKind::Function | UnitKind::Method)
            && parts.len() == 1
            && parts[0].eq_ignore_ascii_case(&unit.name)
        {
            return Some(StorageRef {
                unit: unit.id.clone(),
                path: vec!["$return".into()],
                ty: unit.return_type.clone()?,
                source: unit.source.clone(),
            });
        }

        let mut scope = Some(unit);
        let mut seen = BTreeSet::new();

        while let Some(current) = scope {
            if !seen.insert(&current.id) {
                return None;
            }

            if let Some(v) = self.storage_member(&current.id, parts[0]) {
                if v.kind == VariableKind::External {
                    let matches: Vec<_> = self
                        .model
                        .units
                        .iter()
                        .filter(|u| u.kind == UnitKind::GlobalVariables)
                        .flat_map(|u| {
                            u.variables
                                .iter()
                                .filter(|g| g.name.eq_ignore_ascii_case(&v.name))
                                .map(move |g| (u, g))
                        })
                        .collect();

                    let [(root, global)] = matches.as_slice() else {
                        return None;
                    };

                    return self.endpoint_member(root, global, &parts[1..]);
                }

                return self.endpoint_member(current, v, &parts[1..]);
            }

            scope = current
                .owner
                .as_ref()
                .and_then(|o| self.model.units.iter().find(|u| &u.id == o));
        }

        if let Resolution::Found(id) = self
            .index
            .lookup(parts[0], |category| matches!(category, Category::Unit(_)))
        {
            let root = self.model.units.iter().find(|u| u.id == id)?;
            let v = root.variables.iter().find(|v| {
                parts
                    .get(1)
                    .is_some_and(|name| v.name.eq_ignore_ascii_case(name))
            })?;
            return self.endpoint_member(root, v, &parts[2..]);
        }

        let id = self.resolve(unit, parts[0])?;

        for root in &self.model.units {
            if let Some(v) = root.variables.iter().find(|v| v.id == id) {
                return self.endpoint_member(root, v, &parts[1..]);
            }
        }

        None
    }

    fn endpoint_member(
        &self,
        root: &Unit,
        variable: &Variable,
        members: &[&str],
    ) -> Option<StorageRef> {
        let mut path = vec![variable.name.clone()];
        let mut current = variable;

        for member in members {
            let ty = self.model.effective_type(&current.ty)?;

            if !ty.dimensions.is_empty() || ty.indirection.is_some() {
                return None;
            }

            let width = bit_width(ty.base.as_str());

            if member.parse::<u8>().is_ok_and(|bit| bit < width) {
                path.push((*member).into());
                return Some(StorageRef {
                    unit: root.id.clone(),
                    path,
                    ty: TypeRef::scalar("BOOL"),
                    source: current.source.clone(),
                });
            }

            let target = ty.target.as_deref()?;

            if let Some(component) = self.model.units.iter().find(|u| u.id == target) {
                current = self.storage_member(&component.id, member)?;
            } else if let Some(record) = self.model.types.iter().find(|t| t.id == target) {
                current = record
                    .fields()
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(member))?;
            } else {
                return None;
            }

            path.push(current.name.clone());
        }

        Some(StorageRef {
            unit: root.id.clone(),
            path,
            ty: current.ty.clone(),
            source: current.source.clone(),
        })
    }

    pub(crate) fn callable_name(&self, unit: &Unit, callee: &Expr) -> Option<&'a Unit> {
        let target = self.resolve(unit, &callee.text())?;
        self.callable(&target, &mut BTreeSet::new())
    }

    pub(crate) fn enum_value(&self, unit: &Unit, name: &str) -> Option<i128> {
        let target = self.resolve(unit, name)?;

        for ty in &self.model.types {
            if !ty
                .literals()
                .iter()
                .any(|literal| id(&["enum_literal", &ty.id, &literal.name]) == target)
            {
                continue;
            }

            let mut next = 0i128;

            for literal in ty.literals() {
                let number = literal
                    .value
                    .as_ref()
                    .map(|v| v.parse::<i128>().ok())
                    .unwrap_or(Some(next))?;

                if id(&["enum_literal", &ty.id, &literal.name]) == target {
                    return Some(number);
                }

                next = number.checked_add(1)?;
            }
        }

        None
    }

    fn storage_member(&self, owner: &str, name: &str) -> Option<&Variable> {
        let target = self.member(owner, name, &mut BTreeSet::new())?;

        self.model
            .units
            .iter()
            .flat_map(|u| &u.variables)
            .chain(self.model.types.iter().flat_map(|t| t.fields()))
            .find(|v| v.id == target)
    }

    fn resolve(&self, unit: &Unit, name: &str) -> Option<String> {
        let parts: Vec<_> = name.split('.').collect();
        let first = parts[0];
        let mut scope = Some(unit);
        let mut found = None;
        let mut visited = BTreeSet::new();

        while let Some(current) = scope {
            if !visited.insert(&current.id) {
                return None;
            }

            if let Some(v) = current
                .variables
                .iter()
                .find(|v| v.name.eq_ignore_ascii_case(first))
            {
                found = Some(v.id.clone());
                break;
            }

            if let Some(child) = self.model.units.iter().find(|u| {
                u.owner.as_deref() == Some(&current.id) && u.name.eq_ignore_ascii_case(first)
            }) {
                found = Some(child.id.clone());
                break;
            }

            scope = current
                .owner
                .as_ref()
                .and_then(|id| self.model.units.iter().find(|u| &u.id == id));
        }

        let mut resolution = if let Some(found) = found {
            Resolution::Found(found)
        } else {
            self.index.lookup(first, |_| true)
        };

        if matches!(resolution, Resolution::Missing) {
            let candidates = self
                .model
                .units
                .iter()
                .filter(|u| u.kind == UnitKind::GlobalVariables)
                .flat_map(|u| &u.variables)
                .filter(|v| v.name.eq_ignore_ascii_case(first))
                .map(|v| v.id.as_str());
            resolution = choose(candidates);
        }

        let mut target = resolution.target()?;
        for member in &parts[1..] {
            target = self.member(&target, member, &mut BTreeSet::new())?;
        }
        Some(target)
    }

    fn member(&self, owner: &str, name: &str, visited: &mut BTreeSet<String>) -> Option<String> {
        if !visited.insert(owner.into()) {
            return None;
        }

        for unit in &self.model.units {
            if unit.id == owner {
                if let Some(v) = unit
                    .variables
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(name))
                {
                    return Some(v.id.clone());
                }

                if let Some(child) = self.model.units.iter().find(|u| {
                    u.owner.as_deref() == Some(owner) && u.name.eq_ignore_ascii_case(name)
                }) {
                    return Some(child.id.clone());
                }

                if let Some(base) = &unit.base_type {
                    return self.member(base, name, visited);
                }
            }

            if let Some(variable) = unit.variables.iter().find(|v| v.id == owner) {
                let width = bit_width(&variable.ty.base);

                if name.parse::<u8>().is_ok_and(|bit| bit < width) {
                    return Some(variable.id.clone());
                }

                return variable
                    .ty
                    .target
                    .as_ref()
                    .and_then(|id| self.member(id, name, visited));
            }
        }

        for ty in &self.model.types {
            if ty.id == owner {
                if let Some(v) = ty
                    .fields()
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(name))
                {
                    return Some(v.id.clone());
                }

                if let Some(value) = ty
                    .literals()
                    .iter()
                    .find(|v| v.name.eq_ignore_ascii_case(name))
                {
                    return Some(id(&["enum_literal", &ty.id, &value.name]));
                }

                return ty
                    .base()
                    .and_then(|b| b.target.as_ref())
                    .and_then(|id| self.member(id, name, visited));
            }

            if let Some(field) = ty.fields().iter().find(|v| v.id == owner) {
                return field
                    .ty
                    .target
                    .as_ref()
                    .and_then(|id| self.member(id, name, visited));
            }
        }

        None
    }

    fn callable(&self, id: &str, visited: &mut BTreeSet<String>) -> Option<&'a Unit> {
        if !visited.insert(id.into()) {
            return None;
        }

        if let Some(unit) = self.model.units.iter().find(|u| u.id == id) {
            return Some(unit);
        }

        let target = self
            .model
            .units
            .iter()
            .flat_map(|u| &u.variables)
            .chain(self.model.types.iter().flat_map(|t| t.fields()))
            .find(|v| v.id == id)
            .and_then(|v| v.ty.target.as_deref())
            .or_else(|| {
                self.model
                    .types
                    .iter()
                    .find(|t| t.id == id)
                    .and_then(|t| t.base())
                    .and_then(|b| b.target.as_deref())
            })?;

        self.callable(target, visited)
    }
}

fn bit_width(base: &str) -> u8 {
    match base.to_ascii_uppercase().as_str() {
        "BYTE" | "SINT" | "USINT" => 8,
        "WORD" | "INT" | "UINT" => 16,
        "DWORD" | "DINT" | "UDINT" => 32,
        "LWORD" | "LINT" | "ULINT" => 64,
        _ => 0,
    }
}

fn has_index(expression: &Expr) -> bool {
    match expression {
        Expr::Index { .. } => true,
        Expr::Member { base, .. } => has_index(base),
        _ => false,
    }
}

fn reference_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Reference { name } => Some(name.clone()),
        Expr::Member { base, name } => Some(format!("{}.{name}", reference_name(base)?)),
        Expr::Index { base, .. } => reference_name(base),
        _ => None,
    }
}
