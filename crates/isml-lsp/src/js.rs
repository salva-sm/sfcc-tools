//! JavaScript read with tree-sitter: what a script requires, what it binds that to, and what
//! it exports. A template's `${...}` expressions and `<isscript>` blocks are parsed in place,
//! the markup around them blanked, so every byte offset still points into the template.
//!
//! Names are scoped by function: `var helpers` in one function is not `helpers` in another.
//! Block scoping of `let` and `const` is not modelled; nor is a variable reassigned after its
//! declaration, which keeps the value it was declared with.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Node, Parser, Tree};

/// How deep `var a = b; var b = c; ...` is followed before giving up.
const MAX_ALIASES: usize = 8;

/// The text as parsed, and its tree.
pub struct Parsed {
    code: String,
    tree: Tree,
}

impl Parsed {
    /// `None` for anything but a script or a template.
    pub fn new(path: &Path, text: &str) -> Option<Parsed> {
        let code = match path.extension()?.to_str()? {
            "js" | "ds" => text.to_string(),
            "isml" => template_code(text),
            _ => return None,
        };
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .ok()?;
        let tree = parser.parse(&code, None)?;
        Some(Parsed { code, tree })
    }

    pub fn script(&self) -> Script<'_> {
        Script::new(&self.code, self.tree.root_node())
    }
}

/// Where a module comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// `require('spec')`
    Require(String),
    /// `module.superModule`
    Super,
}

/// What an expression stands for, as far as the text says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Module(Origin),
    /// One of a module's exports: `require(...).total`, or `total` out of
    /// `var { total } = require(...)`.
    Export(Origin, String),
}

/// What a file does with one name it could export.
#[derive(Debug, Default, PartialEq, Eq)]
pub enum Export {
    /// Defines it itself.
    Defines(Definition),
    /// Hands the parent's on: `module.exports = base`, `total: base.total`.
    PassesOn,
    /// Exports an object without it, so whatever reaches this file finds nothing.
    Absent,
    /// Exports something the text does not say enough about.
    #[default]
    Unknown,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Definition {
    /// Byte ranges: the exported key or property, and the declaration it names.
    pub at: Vec<(usize, usize)>,
    /// Where the declarations it names start, so their uses in the file can be told apart.
    declarations: Vec<usize>,
    /// The identifiers handed to the export (`total: total`): not uses of it.
    values: Vec<usize>,
    /// Where `base.total =` assigns it, when the file defines it on its parent.
    on_parent: Vec<usize>,
    /// Each place it is defined: the name to jump to, and the function written there.
    pub sites: Vec<Site>,
}

/// One definition of an export: the declaration its value names (`function total`) when
/// there is one, otherwise the exported key or property itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// Byte range of the name.
    pub name: (usize, usize),
    /// Byte range of the function, when the definition is one written out there.
    function: Option<(usize, usize)>,
}

/// What a definition says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// `total(basket, options)`, called by the exported name.
    pub signature: Option<String>,
    /// Each parameter as written, and the name a `@param` would give it.
    pub parameters: Vec<(String, Option<String>)>,
    /// The `/** */` block right above it, stars and indentation stripped.
    pub doc: Option<String>,
}

/// `module.exports = value`, the variable `value` names when it is one, and the object
/// literal or call behind it.
#[derive(Clone, Copy)]
struct Exported<'a> {
    value: Option<Node<'a>>,
    local: Option<Node<'a>>,
    object: Option<Node<'a>>,
}

pub struct Script<'a> {
    code: &'a str,
    root: Node<'a>,
    /// The names each function (and the file) declares, by the scope node's id.
    scopes: HashMap<usize, HashMap<&'a str, Node<'a>>>,
    /// What each declared name was bound to, worked out once: a file reads the same
    /// variables over and over.
    bound: RefCell<HashMap<usize, Option<Value>>>,
    /// Every node, in source order, and each one's parent: tree-sitter finds a parent by
    /// walking down from the root, too slow to do for every name read.
    nodes: Vec<Node<'a>>,
    parents: HashMap<usize, Node<'a>>,
    /// The assignments, where exports are made: a file is asked about each name it could
    /// export, and walking every node for each would be most of a completion's cost.
    assignments: Vec<Node<'a>>,
    exported: OnceCell<Exported<'a>>,
    /// Whether `exports` is written to anywhere.
    writes_exports: OnceCell<bool>,
}

impl<'a> Script<'a> {
    fn new(code: &'a str, root: Node<'a>) -> Script<'a> {
        let mut script = Script {
            code,
            root,
            scopes: HashMap::new(),
            bound: RefCell::new(HashMap::new()),
            nodes: Vec::new(),
            parents: HashMap::new(),
            assignments: Vec::new(),
            exported: OnceCell::new(),
            writes_exports: OnceCell::new(),
        };
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            script.nodes.push(node);
            if node.kind() == "assignment_expression" {
                script.assignments.push(node);
            }
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            for child in children.iter().rev() {
                script.parents.insert(child.id(), node);
                pending.push(*child);
            }
        }
        script.declare_in(root, root);
        script
    }

    pub fn text(&self, node: Node) -> &'a str {
        &self.code[node.byte_range()]
    }

    /// The identifier at `offset`, or just before it when the cursor sits at its end.
    pub fn name_at(&self, offset: usize) -> Option<Node<'a>> {
        [offset, offset.saturating_sub(1)]
            .into_iter()
            .find_map(|at| {
                let node = self.root.named_descendant_for_byte_range(at, at)?;
                is_name(node.kind()).then_some(node)
            })
    }

    /// The smallest named node holding the last character typed before `offset`, spaces
    /// aside: what a cursor at `offset` is in the middle of.
    pub fn node_at(&self, offset: usize) -> Option<Node<'a>> {
        let typed = self.code[..offset.min(self.code.len())].trim_end();
        let before = typed.chars().next_back()?;
        let at = typed.len() - before.len_utf8();
        self.root.named_descendant_for_byte_range(at, at)
    }

    /// Every node of the file, in source order.
    pub fn nodes(&self) -> &[Node<'a>] {
        &self.nodes
    }

    pub fn parent(&self, node: Node<'a>) -> Option<Node<'a>> {
        self.parents.get(&node.id()).copied()
    }

    fn declare_in(&mut self, node: Node<'a>, scope: Node<'a>) {
        let mut scope = scope;
        match node.kind() {
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.declare(scope, name);
                }
                if node.kind() != "class_declaration" {
                    scope = node;
                    self.declare_parameters(node);
                }
            }
            "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition" => {
                scope = node;
                if node.kind() != "method_definition" {
                    if let Some(name) = node.child_by_field_name("name") {
                        self.declare(scope, name);
                    }
                }
                self.declare_parameters(node);
            }
            "catch_clause" => {
                scope = node;
                if let Some(parameter) = node.child_by_field_name("parameter") {
                    self.declare_pattern(scope, parameter);
                }
            }
            "variable_declarator" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.declare_pattern(scope, name);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        for child in children {
            self.declare_in(child, scope);
        }
    }

    fn declare_parameters(&mut self, function: Node<'a>) {
        if let Some(single) = function.child_by_field_name("parameter") {
            self.declare(function, single);
        }
        if let Some(parameters) = function.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            let children: Vec<Node> = parameters.named_children(&mut cursor).collect();
            for parameter in children {
                self.declare_pattern(function, parameter);
            }
        }
    }

    /// The names a binding pattern declares: `a`, `{ a, b: c, d = 1 }`, `[a, ...b]`.
    fn declare_pattern(&mut self, scope: Node<'a>, pattern: Node<'a>) {
        match pattern.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => self.declare(scope, pattern),
            "pair_pattern" => {
                if let Some(value) = pattern.child_by_field_name("value") {
                    self.declare_pattern(scope, value);
                }
            }
            "object_assignment_pattern" | "assignment_pattern" => {
                if let Some(left) = pattern.child_by_field_name("left") {
                    self.declare_pattern(scope, left);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                let mut cursor = pattern.walk();
                let children: Vec<Node> = pattern.named_children(&mut cursor).collect();
                for child in children {
                    self.declare_pattern(scope, child);
                }
            }
            _ => {}
        }
    }

    fn declare(&mut self, scope: Node<'a>, name: Node<'a>) {
        let text = self.text(name);
        self.scopes
            .entry(scope.id())
            .or_default()
            .entry(text)
            .or_insert(name);
    }

    /// Where the name `node` reads is declared, looking outwards from it: the declaration
    /// itself for a name being declared.
    pub fn declaration_of(&self, node: Node<'a>) -> Option<Node<'a>> {
        let name = self.text(node);
        let mut scope = Some(node);
        while let Some(current) = scope {
            if let Some(found) = self
                .scopes
                .get(&current.id())
                .and_then(|names| names.get(name))
            {
                return Some(*found);
            }
            scope = self.parent(current);
        }
        None
    }

    fn is_declaration(&self, node: Node<'a>) -> bool {
        self.declaration_of(node)
            .is_some_and(|found| found.id() == node.id())
    }

    /// `require('spec')` or `module.superModule`, through parentheses.
    pub fn module(&self, node: Node<'a>) -> Option<Origin> {
        match node.kind() {
            "parenthesized_expression" => self.module(node.named_child(0)?),
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                if function.kind() != "identifier" || self.text(function) != "require" {
                    return None;
                }
                let spec = node.child_by_field_name("arguments")?.named_child(0)?;
                literal(self.text(spec), spec).map(Origin::Require)
            }
            "member_expression" => {
                let object = node.child_by_field_name("object")?;
                let property = node.child_by_field_name("property")?;
                (object.kind() == "identifier"
                    && self.text(object) == "module"
                    && self.text(property) == "superModule")
                    .then_some(Origin::Super)
            }
            _ => None,
        }
    }

    /// What `node` stands for: a module, or one of its exports, followed through the
    /// variables it was declared into.
    pub fn value_of(&self, node: Node<'a>) -> Option<Value> {
        self.value_within(node, 0)
    }

    fn value_within(&self, node: Node<'a>, depth: usize) -> Option<Value> {
        if depth > MAX_ALIASES {
            return None;
        }
        if let Some(origin) = self.module(node) {
            return Some(Value::Module(origin));
        }
        match node.kind() {
            "parenthesized_expression" => self.value_within(node.named_child(0)?, depth + 1),
            "member_expression" => {
                let object = node.child_by_field_name("object")?;
                let property = node.child_by_field_name("property")?;
                match self.value_within(object, depth + 1)? {
                    Value::Module(origin) => {
                        Some(Value::Export(origin, self.text(property).to_string()))
                    }
                    Value::Export(..) => None,
                }
            }
            "identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern" => {
                let declaration = self.declaration_of(node)?;
                if let Some(known) = self.bound.borrow().get(&declaration.id()) {
                    return known.clone();
                }
                let value = self.bound_value(declaration, depth + 1);
                self.bound
                    .borrow_mut()
                    .insert(declaration.id(), value.clone());
                value
            }
            "property_identifier" => {
                // The key of `{ total: renamed } = require(...)` names the export.
                let pair = self
                    .parent(node)
                    .filter(|parent| parent.kind() == "pair_pattern")?;
                let source = self.destructured_from(pair, depth)?;
                Some(Value::Export(source, self.text(node).to_string()))
            }
            _ => None,
        }
    }

    /// The export of a module that `node` uses: `total` in `helpers.total` or
    /// `require(...).total`, a destructured `total`, or an alias of one. Not the file's own
    /// `base.total` once it has replaced it, nor `base.total =` replacing it.
    pub fn export_at(&self, node: Node<'a>) -> Option<(Origin, String)> {
        let member = self.parent(node).filter(|parent| {
            parent.kind() == "member_expression"
                && parent
                    .child_by_field_name("property")
                    .is_some_and(|property| property.id() == node.id())
        });
        let Some(member) = member else {
            return match self.value_of(node)? {
                Value::Export(origin, key) => Some((origin, key)),
                Value::Module(_) => None,
            };
        };
        let Value::Module(origin) = self.value_of(member.child_by_field_name("object")?)? else {
            return None;
        };
        let name = self.text(node);
        if origin == Origin::Super {
            let own = match self.export(name) {
                Export::Defines(definition) => definition.on_parent,
                _ => Vec::new(),
            };
            if self.is_assigned(member) || self.reads_own(member, &own) {
                return None;
            }
        }
        Some((origin, name.to_string()))
    }

    /// What a declared name was bound to.
    fn bound_value(&self, declaration: Node<'a>, depth: usize) -> Option<Value> {
        let parent = self.parent(declaration)?;
        if parent.kind() == "variable_declarator"
            && parent
                .child_by_field_name("name")
                .is_some_and(|name| name.id() == declaration.id())
        {
            return self.value_within(parent.child_by_field_name("value")?, depth);
        }
        // `{ total }`, `{ total = x }` or `{ total: renamed }`, one level deep.
        let (entry, key) = match parent.kind() {
            "object_pattern" => (declaration, self.text(declaration)),
            "object_assignment_pattern" => (parent, self.text(declaration)),
            "pair_pattern" => (parent, self.text(parent.child_by_field_name("key")?)),
            _ => return None,
        };
        let source = self.destructured_from(entry, depth)?;
        Some(Value::Export(source, key.to_string()))
    }

    /// The module an entry of `var { ... } = module` destructures.
    fn destructured_from(&self, entry: Node<'a>, depth: usize) -> Option<Origin> {
        let pattern = self
            .parent(entry)
            .filter(|parent| parent.kind() == "object_pattern")?;
        let declarator = self
            .parent(pattern)
            .filter(|parent| parent.kind() == "variable_declarator")?;
        match self.value_within(declarator.child_by_field_name("value")?, depth + 1)? {
            Value::Module(origin) => Some(origin),
            Value::Export(..) => None,
        }
    }

    /// What this file does with `name`, as an export.
    pub fn export(&self, name: &str) -> Export {
        let mut definition = Definition::default();
        let mut passes_on = false;
        let Exported {
            value: exported,
            local,
            object,
        } = self.exported();

        // `exports.total =`, `module.exports.total =`, `helpers.total =`, `base.total =`.
        for node in &self.assignments {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                continue;
            };
            if left.kind() != "member_expression" {
                continue;
            }
            let (Some(object), Some(property)) = (
                left.child_by_field_name("object"),
                left.child_by_field_name("property"),
            ) else {
                continue;
            };
            if self.text(property) != name {
                continue;
            }
            let on_parent = self.value_of(object) == Some(Value::Module(Origin::Super));
            let on_exports = self.receives_exports(object, exported, local);
            if !on_parent && !on_exports {
                continue;
            }
            if self.passes_parent_on(right, name) {
                passes_on = true;
                continue;
            }
            if on_parent {
                definition.on_parent.push(property.start_byte());
            }
            self.define(&mut definition, property, Some(right));
        }

        let mut absent = false;
        if let Some(value) = exported {
            match object {
                Some(object) => match self.exported_object(object, name, &mut definition) {
                    Some(true) => passes_on = true,
                    Some(false) => absent = true,
                    None => {}
                },
                None => {
                    if self.value_of(value) == Some(Value::Module(Origin::Super)) {
                        passes_on = true;
                    }
                }
            }
        } else if *self
            .writes_exports
            .get_or_init(|| self.nodes.iter().any(|node| self.is_exports(*node)))
        {
            // Only `exports.x = ...` elsewhere: an object without this name.
            absent = true;
        }

        if !definition.at.is_empty() {
            Export::Defines(definition)
        } else if passes_on {
            Export::PassesOn
        } else if absent {
            Export::Absent
        } else {
            Export::Unknown
        }
    }

    /// What `module.exports` is set to: the last assignment, which is what callers get.
    fn exported(&self) -> Exported<'a> {
        *self.exported.get_or_init(|| self.find_exported())
    }

    fn find_exported(&self) -> Exported<'a> {
        let value = self
            .assignments
            .iter()
            .filter_map(|node| {
                let left = node.child_by_field_name("left")?;
                self.is_module_exports(left)
                    .then(|| node.child_by_field_name("right"))
                    .flatten()
            })
            .next_back();
        // The local object `module.exports = helpers` hands out, if that is what it is.
        let local = value
            .filter(|value| value.kind() == "identifier")
            .and_then(|value| self.declaration_of(value));
        let object = match value.map(|value| value.kind()) {
            Some("identifier") => local
                .and_then(|local| self.parent(local))
                .filter(|parent| parent.kind() == "variable_declarator")
                .and_then(|declarator| declarator.child_by_field_name("value")),
            _ => value,
        };
        Exported {
            value,
            local,
            object,
        }
    }

    /// Every name this file exports itself or hands on from its parent, in the order it
    /// writes them, and whether it also hands on whatever else its parent exports
    /// (`module.exports = base`, `Object.create(base)`, `{ ...base }`).
    pub fn exported_names(&self) -> (Vec<String>, bool) {
        let Exported {
            value,
            local,
            object,
        } = self.exported();
        let mut candidates: Vec<&'a str> = Vec::new();
        let mut inherits = false;
        let mut objects: Vec<Node<'a>> = object.into_iter().collect();
        while let Some(object) = objects.pop() {
            if self.value_of(object) == Some(Value::Module(Origin::Super)) {
                inherits = true;
                continue;
            }
            match object.kind() {
                "object" => {
                    let mut cursor = object.walk();
                    let entries: Vec<Node> = object.named_children(&mut cursor).collect();
                    for entry in entries {
                        let key = match entry.kind() {
                            "pair" => entry.child_by_field_name("key"),
                            "method_definition" => entry.child_by_field_name("name"),
                            "shorthand_property_identifier" => Some(entry),
                            "spread_element" => {
                                objects.extend(entry.named_child(0));
                                None
                            }
                            _ => None,
                        };
                        if let Some(key) = key {
                            candidates.push(key_text(self.text(key)));
                        }
                    }
                }
                "call_expression" => {
                    let callee = object
                        .child_by_field_name("function")
                        .map(|function| self.text(function));
                    if matches!(callee, Some("Object.create" | "Object.assign")) {
                        if let Some(arguments) = object.child_by_field_name("arguments") {
                            let mut cursor = arguments.walk();
                            objects.extend(arguments.named_children(&mut cursor));
                        }
                    }
                }
                _ => {}
            }
        }
        for node in &self.assignments {
            let Some(left) = node
                .child_by_field_name("left")
                .filter(|left| left.kind() == "member_expression")
            else {
                continue;
            };
            let (Some(object), Some(property)) = (
                left.child_by_field_name("object"),
                left.child_by_field_name("property"),
            ) else {
                continue;
            };
            let on_exports = self.receives_exports(object, value, local)
                || self.value_of(object) == Some(Value::Module(Origin::Super));
            if on_exports {
                candidates.push(self.text(property));
            }
        }
        let mut names: Vec<String> = Vec::new();
        for candidate in candidates {
            if names.iter().any(|name| name == candidate) {
                continue;
            }
            if matches!(
                self.export(candidate),
                Export::Defines(_) | Export::PassesOn
            ) {
                names.push(candidate.to_string());
            }
        }
        (names, inherits)
    }

    /// What the object a file exports does with `name`: `Some(true)` when it hands the
    /// parent's on, `Some(false)` when it leaves it out, `None` when it defines it or the
    /// object is not one the text spells out.
    fn exported_object(
        &self,
        object: Node<'a>,
        name: &str,
        definition: &mut Definition,
    ) -> Option<bool> {
        if self.value_of(object) == Some(Value::Module(Origin::Super)) {
            return Some(true);
        }
        match object.kind() {
            "object" => {
                let mut cursor = object.walk();
                let entries: Vec<Node> = object.named_children(&mut cursor).collect();
                let mut found = false;
                let mut inherits = false;
                for entry in entries {
                    match entry.kind() {
                        "pair" => {
                            let (Some(key), Some(value)) = (
                                entry.child_by_field_name("key"),
                                entry.child_by_field_name("value"),
                            ) else {
                                continue;
                            };
                            if key_text(self.text(key)) != name {
                                continue;
                            }
                            found = true;
                            if self.passes_parent_on(value, name) {
                                inherits = true;
                            } else {
                                self.define(definition, key, Some(value));
                            }
                        }
                        "shorthand_property_identifier" if self.text(entry) == name => {
                            found = true;
                            self.define(definition, entry, Some(entry));
                        }
                        "method_definition" => {
                            let key = entry.child_by_field_name("name")?;
                            if key_text(self.text(key)) == name {
                                found = true;
                                self.define(definition, key, None);
                            }
                        }
                        "spread_element" => {
                            let spread = entry.named_child(0)?;
                            if self.value_of(spread) == Some(Value::Module(Origin::Super)) {
                                inherits = true;
                            }
                        }
                        _ => {}
                    }
                }
                match (found, inherits) {
                    (_, true) if definition.at.is_empty() => Some(true),
                    (false, false) if definition.at.is_empty() => Some(false),
                    _ => None,
                }
            }
            // `Object.create(base)` and `Object.assign({}, base, { ... })`.
            "call_expression" => {
                let function = object.child_by_field_name("function")?;
                let callee = self.text(function);
                if callee != "Object.create" && callee != "Object.assign" {
                    return None;
                }
                let arguments = object.child_by_field_name("arguments")?;
                let mut cursor = arguments.walk();
                let arguments: Vec<Node> = arguments.named_children(&mut cursor).collect();
                let mut inherits = false;
                for argument in arguments {
                    if self.value_of(argument) == Some(Value::Module(Origin::Super)) {
                        inherits = true;
                    } else if argument.kind() == "object" {
                        self.exported_object(argument, name, definition);
                    }
                }
                (inherits && definition.at.is_empty()).then_some(true)
            }
            _ => None,
        }
    }

    /// Records the key that exports `name`, and the function or variable its value names.
    fn define(&self, definition: &mut Definition, key: Node<'a>, value: Option<Node<'a>>) {
        definition.at.push((key.start_byte(), key.end_byte()));
        let range = |node: Node| (node.start_byte(), node.end_byte());
        let Some(value) = value else {
            // `total() {}` in an object: the method is the function.
            let method = self
                .parent(key)
                .filter(|parent| parent.kind() == "method_definition");
            definition.sites.push(Site {
                name: range(key),
                function: method.map(range),
            });
            return;
        };
        if !matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
            definition.sites.push(Site {
                name: range(key),
                function: is_function(value.kind()).then(|| range(value)),
            });
            return;
        }
        definition.values.push(value.start_byte());
        match self.declaration_of(value) {
            Some(declaration) if declaration.id() != value.id() => {
                definition.at.push(range(declaration));
                definition.declarations.push(declaration.start_byte());
                definition.sites.push(Site {
                    name: range(declaration),
                    function: self.declared_function(declaration).map(range),
                });
            }
            _ => definition.sites.push(Site {
                name: range(key),
                function: None,
            }),
        }
    }

    /// `function total() {}`, or `var total = function () {}` / an arrow.
    fn declared_function(&self, declaration: Node<'a>) -> Option<Node<'a>> {
        let parent = self.parent(declaration)?;
        match parent.kind() {
            "function_declaration" | "generator_function_declaration" => Some(parent),
            "variable_declarator" => parent
                .child_by_field_name("value")
                .filter(|value| is_function(value.kind())),
            _ => None,
        }
    }

    /// The signature and documentation of a definition, under the name it is exported as.
    pub fn describe(&self, site: &Site, name: &str) -> Description {
        let node = |(start, end): (usize, usize)| self.root.descendant_for_byte_range(start, end);
        let function = site.function.and_then(node);
        let parameters: Vec<(String, Option<String>)> = function
            .map(|function| {
                let nodes: Vec<Node> = match function.child_by_field_name("parameters") {
                    Some(list) => {
                        let mut cursor = list.walk();
                        list.named_children(&mut cursor)
                            .filter(|parameter| parameter.kind() != "comment")
                            .collect()
                    }
                    None => function
                        .child_by_field_name("parameter")
                        .into_iter()
                        .collect(),
                };
                nodes
                    .into_iter()
                    .map(|parameter| {
                        let label = self
                            .text(parameter)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        let named = match parameter.kind() {
                            "identifier" => Some(parameter),
                            "assignment_pattern" => parameter
                                .child_by_field_name("left")
                                .filter(|left| left.kind() == "identifier"),
                            _ => None,
                        };
                        (label, named.map(|named| self.text(named).to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let signature = function.map(|_| {
            let labels: Vec<&str> = parameters.iter().map(|(label, _)| label.as_str()).collect();
            format!("{name}({})", labels.join(", "))
        });
        let doc = node(site.name).and_then(|name| self.doc_of(name));
        Description {
            signature,
            parameters,
            doc,
        }
    }

    /// The `/** */` comment directly above the statement, object entry or method that
    /// holds `name`. A plain `/* */` or `//` comment is not documentation, and nor is one
    /// with code between it and the definition.
    fn doc_of(&self, name: Node<'a>) -> Option<String> {
        let mut item = name;
        while let Some(parent) = self.parent(item) {
            if matches!(
                parent.kind(),
                "program" | "statement_block" | "object" | "class_body"
            ) {
                break;
            }
            item = parent;
        }
        let comment = item.prev_named_sibling()?;
        let text = self.text(comment);
        if comment.kind() != "comment"
            || !text.starts_with("/**")
            || !self.code[comment.end_byte()..item.start_byte()]
                .trim()
                .is_empty()
        {
            return None;
        }
        let body = text.strip_prefix("/**")?.strip_suffix("*/")?;
        let lines: Vec<&str> = body
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                let stripped = trimmed.strip_prefix('*').unwrap_or(trimmed);
                stripped.strip_prefix(' ').unwrap_or(stripped)
            })
            .collect();
        let doc = lines.join("\n").trim().to_string();
        (!doc.is_empty()).then_some(doc)
    }

    /// `base.total`, with `base` the parent module: the parent's own function, unchanged.
    fn passes_parent_on(&self, value: Node<'a>, name: &str) -> bool {
        self.value_of(value) == Some(Value::Export(Origin::Super, name.to_string()))
    }

    /// `module.exports`
    fn is_module_exports(&self, node: Node<'a>) -> bool {
        node.kind() == "member_expression"
            && node
                .child_by_field_name("object")
                .is_some_and(|object| self.text(object) == "module")
            && node
                .child_by_field_name("property")
                .is_some_and(|property| self.text(property) == "exports")
    }

    /// Whether a property set on `object` ends up exported: `module.exports.x`, the local
    /// object `module.exports` is set to, or `exports.x` as long as `module.exports` is not
    /// replaced (which leaves `exports` pointing at an object nobody gets).
    fn receives_exports(
        &self,
        object: Node<'a>,
        exported: Option<Node<'a>>,
        local: Option<Node<'a>>,
    ) -> bool {
        if self.is_module_exports(object) {
            return true;
        }
        if self.is_exports(object) {
            return exported.is_none();
        }
        local.is_some_and(|local| {
            self.declaration_of(object)
                .is_some_and(|found| found.id() == local.id())
        })
    }

    /// `exports` or `module.exports`, as the receiver of a property.
    fn is_exports(&self, node: Node<'a>) -> bool {
        self.is_module_exports(node)
            || node.kind() == "identifier"
                && self.text(node) == "exports"
                && self.declaration_of(node).is_none()
    }

    /// Byte ranges where this file uses the `name` some module exports, through a
    /// `require` or `module.superModule` that `reaches` accepts.
    pub fn uses(&self, name: &str, reaches: impl Fn(&Origin) -> bool) -> Vec<(usize, usize)> {
        let own = match self.export(name) {
            Export::Defines(definition) => definition.on_parent,
            _ => Vec::new(),
        };
        let mut verdicts: HashMap<Origin, bool> = HashMap::new();
        let mut reached = |origin: &Origin| {
            *verdicts
                .entry(origin.clone())
                .or_insert_with(|| reaches(origin))
        };
        let mut found = Vec::new();
        for &node in self.nodes() {
            let range = (node.start_byte(), node.end_byte());
            match node.kind() {
                "member_expression" => {
                    let (Some(object), Some(property)) = (
                        node.child_by_field_name("object"),
                        node.child_by_field_name("property"),
                    ) else {
                        continue;
                    };
                    if self.text(property) != name || self.is_assigned(node) {
                        continue;
                    }
                    let Some(Value::Module(origin)) = self.value_of(object) else {
                        continue;
                    };
                    if origin == Origin::Super && self.reads_own(node, &own) {
                        continue;
                    }
                    if reached(&origin) {
                        found.push((property.start_byte(), property.end_byte()));
                    }
                }
                // A destructured or aliased export: `total()` after `var { total } = ...`,
                // and the key of the pattern itself.
                "identifier"
                | "shorthand_property_identifier"
                | "shorthand_property_identifier_pattern"
                | "property_identifier" => {
                    if node.kind() == "property_identifier"
                        && self
                            .parent(node)
                            .is_none_or(|parent| parent.kind() != "pair_pattern")
                    {
                        continue;
                    }
                    // The local name in `{ total: renamed }` (the key is the use), and the
                    // `t` of `var t = helpers.total` (the member is).
                    if node.kind() == "identifier"
                        && (self
                            .parent(node)
                            .is_some_and(|parent| parent.kind() == "pair_pattern")
                            || self.is_declaration(node))
                    {
                        continue;
                    }
                    let Some(Value::Export(origin, key)) = self.value_of(node) else {
                        continue;
                    };
                    if key == name && reached(&origin) {
                        found.push(range);
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// A read of `base.total` in a file that assigns `base.total` itself gets the file's
    /// own function once that assignment has run: from inside any function, or after it.
    fn reads_own(&self, member: Node<'a>, own: &[usize]) -> bool {
        !own.is_empty()
            && (self.inside_function(member) || own.iter().any(|at| *at < member.start_byte()))
    }

    /// Byte ranges where the file that defines `name` uses its own definition: calls of the
    /// function it exports, and `exports.total()` or `base.total()` reading it back.
    pub fn local_uses(&self, name: &str) -> Vec<(usize, usize)> {
        let Export::Defines(definition) = self.export(name) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for &node in self.nodes() {
            match node.kind() {
                "identifier" | "shorthand_property_identifier" => {
                    if definition.values.contains(&node.start_byte()) || self.is_declaration(node) {
                        continue;
                    }
                    if self
                        .declaration_of(node)
                        .is_some_and(|found| definition.declarations.contains(&found.start_byte()))
                    {
                        found.push((node.start_byte(), node.end_byte()));
                    }
                }
                "member_expression" => {
                    let (Some(object), Some(property)) = (
                        node.child_by_field_name("object"),
                        node.child_by_field_name("property"),
                    ) else {
                        continue;
                    };
                    if self.text(property) != name || self.is_assigned(node) {
                        continue;
                    }
                    let own = self.is_exports(object)
                        || self.value_of(object) == Some(Value::Module(Origin::Super))
                            && self.reads_own(node, &definition.on_parent);
                    if own {
                        found.push((property.start_byte(), property.end_byte()));
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// The left-hand side of an assignment: written, not read.
    fn is_assigned(&self, node: Node<'a>) -> bool {
        self.parent(node).is_some_and(|parent| {
            matches!(
                parent.kind(),
                "assignment_expression" | "augmented_assignment_expression"
            ) && parent
                .child_by_field_name("left")
                .is_some_and(|left| left.id() == node.id())
        })
    }

    fn inside_function(&self, node: Node<'a>) -> bool {
        let mut current = self.parent(node);
        while let Some(parent) = current {
            if matches!(
                parent.kind(),
                "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "generator_function"
                    | "arrow_function"
                    | "method_definition"
            ) {
                return true;
            }
            current = self.parent(parent);
        }
        false
    }
}

fn is_function(kind: &str) -> bool {
    matches!(
        kind,
        "function_expression" | "generator_function" | "arrow_function"
    )
}

fn is_name(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
    )
}

/// `'spec'`, `"spec"` or a template literal with nothing substituted.
fn literal(text: &str, node: Node) -> Option<String> {
    let quoted = match node.kind() {
        "string" => true,
        "template_string" => node.named_child_count() <= 1 && !text.contains("${"),
        _ => false,
    };
    (quoted && text.len() >= 2).then(|| text[1..text.len() - 1].to_string())
}

/// An object key as written: `total`, `'total'` or `"total"`.
fn key_text(text: &str) -> &str {
    text.trim_matches(|c| c == '\'' || c == '"')
}

/// The JavaScript in a template, at the offsets it has there: `${...}` expressions, each
/// closed with `;`, and `<isscript>` blocks. Markup and `<iscomment>` become spaces; line
/// breaks are kept.
fn template_code(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut code: Vec<u8> = bytes
        .iter()
        .map(|byte| match byte {
            b'\n' | b'\r' => *byte,
            _ => b' ',
        })
        .collect();
    // Bytes, not `str`: stepping one at a time would land inside a multi-byte character.
    let lower = text.to_ascii_lowercase().into_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let rest = &lower[index..];
        if rest.starts_with(b"<iscomment") {
            index = find(rest, b"</iscomment>")
                .map_or(bytes.len(), |end| index + end + b"</iscomment>".len());
        } else if rest.starts_with(b"<isscript") {
            let Some(open_end) = find(rest, b">") else {
                break;
            };
            let start = index + open_end + 1;
            let end = find(&lower[start..], b"</isscript>").map_or(bytes.len(), |end| start + end);
            code[start..end].copy_from_slice(&bytes[start..end]);
            index = end;
        } else if rest.starts_with(b"${") {
            let start = index + 2;
            let end = closing_brace(bytes, start).unwrap_or(bytes.len());
            code[start..end].copy_from_slice(&bytes[start..end]);
            if end < bytes.len() {
                code[end] = b';';
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    // Only ASCII was written over whole characters, so this cannot fail.
    String::from_utf8(code).unwrap_or_default()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The `}` that closes an expression starting at `start`, past nested braces and strings.
fn closing_brace(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(_) if byte == b'\\' => index += 1,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'\'' | b'"' | b'`' => quote = Some(byte),
                b'{' => depth += 1,
                b'}' if depth == 0 => return Some(index),
                b'}' => depth -= 1,
                _ => {}
            },
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Parsed {
        Parsed::new(Path::new("x.js"), text).unwrap()
    }

    fn value_at(text: &str, at: &str) -> Option<Value> {
        let parsed = parse(text);
        let script = parsed.script();
        let node = script.name_at(text.find(at).unwrap()).unwrap();
        script.value_of(node)
    }

    fn require(spec: &str) -> Origin {
        Origin::Require(spec.into())
    }

    fn texts(text: &str, ranges: &[(usize, usize)]) -> Vec<String> {
        let mut found: Vec<String> = ranges
            .iter()
            .map(|(start, end)| {
                let line = text[..*start].matches('\n').count();
                format!("{line}:{}", &text[*start..*end])
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn follows_a_module_into_the_variable_it_is_bound_to() {
        let text = "var helpers = require('*/x');\nhelpers.total();";
        assert_eq!(
            value_at(text, "helpers.total"),
            Some(Value::Module(require("*/x")))
        );
        assert_eq!(
            value_at(text, "total()"),
            None,
            "a property is not a variable"
        );
    }

    #[test]
    fn a_member_of_a_require_is_an_export_not_the_module() {
        let text = "var total = require('*/x').total;\ntotal();";
        assert_eq!(
            value_at(text, "total()"),
            Some(Value::Export(require("*/x"), "total".into()))
        );
    }

    #[test]
    fn reads_every_way_of_destructuring_a_require() {
        let text = "var { a, b: renamed, c = 1 } = require('*/x');\na(); renamed(); c();";
        for (at, key) in [("a()", "a"), ("renamed()", "b"), ("c()", "c")] {
            assert_eq!(
                value_at(text, at),
                Some(Value::Export(require("*/x"), key.into())),
                "{at}"
            );
        }
    }

    #[test]
    fn a_name_belongs_to_the_function_that_declares_it() {
        let text = "function a() { var helpers = require('*/x'); }\nfunction b() { var helpers = require('*/y'); helpers.total(); }\nfunction c() { helpers.total(); }";
        let parsed = parse(text);
        let script = parsed.script();
        let in_b = script.name_at(text.find("helpers.total").unwrap()).unwrap();
        assert_eq!(script.value_of(in_b), Some(Value::Module(require("*/y"))));
        let in_c = script
            .name_at(text.rfind("helpers.total").unwrap())
            .unwrap();
        assert_eq!(script.value_of(in_c), None);
    }

    #[test]
    fn a_parameter_hides_a_module_of_the_same_name() {
        let text = "var helpers = require('*/x');\nfunction f(helpers) { helpers.total(); }";
        assert_eq!(value_at(text, "helpers.total"), None);
    }

    #[test]
    fn strings_comments_and_regular_expressions_hold_no_uses() {
        let text = "var helpers = require('*/x');\nvar a = \"helpers.total()\";\n// helpers.total();\nvar re = /'/;\nhelpers.total();";
        let parsed = parse(text);
        let uses = parsed.script().uses("total", |_| true);
        assert_eq!(texts(text, &uses), ["4:total"]);
    }

    #[test]
    fn finds_uses_through_every_kind_of_binding() {
        let text = "var helpers = require('*/x');\nhelpers.total(1);\nrequire('*/x').total(2);\nvar { total } = require('*/x');\ntotal(3);\nvar { total: sum } = require('*/x');\nsum(4);\nvar t = helpers.total;\nt(5);\nhelpers.totalPrice(6);";
        let parsed = parse(text);
        let uses = parsed.script().uses("total", |_| true);
        assert_eq!(
            texts(text, &uses),
            ["1:total", "2:total", "3:total", "4:total", "5:total", "6:sum", "7:total", "8:t"]
        );
    }

    #[test]
    fn a_defining_export_names_its_key_and_its_function() {
        let text = "function total() {}\nmodule.exports = { total: total };";
        let Export::Defines(definition) = parse(text).script().export("total") else {
            panic!("not defined");
        };
        assert_eq!(texts(text, &definition.at), ["0:total", "1:total"]);
    }

    #[test]
    fn tells_what_an_override_does_with_the_parent_function() {
        let export = |text: &str| parse(text).script().export("total");
        let base = "var base = module.superModule;\n";
        assert!(matches!(
            export(&format!(
                "{base}base.total = function () {{}};\nmodule.exports = base;"
            )),
            Export::Defines(_)
        ));
        assert_eq!(
            export(&format!("{base}module.exports = base;")),
            Export::PassesOn
        );
        assert_eq!(
            export(&format!("{base}module.exports = {{ total: base.total }};")),
            Export::PassesOn
        );
        assert_eq!(
            export(&format!(
                "{base}module.exports = Object.assign({{}}, base, {{ other: 1 }});"
            )),
            Export::PassesOn
        );
        assert_eq!(
            export(&format!("{base}module.exports = Object.create(base);")),
            Export::PassesOn
        );
        assert_eq!(
            export(&format!("{base}module.exports = {{ ...base, other: 1 }};")),
            Export::PassesOn
        );
        assert_eq!(
            export(&format!("{base}module.exports = {{ other: base.other }};")),
            Export::Absent
        );
        assert_eq!(export("module.exports = build();"), Export::Unknown);
    }

    #[test]
    fn reads_an_object_built_before_it_is_exported() {
        let text = "var helpers = {};\nhelpers.total = function () {};\nmodule.exports = helpers;";
        assert!(matches!(
            parse(text).script().export("total"),
            Export::Defines(_)
        ));
    }

    #[test]
    fn a_saved_parent_function_is_the_parent_and_a_late_read_is_the_override() {
        let text = "var base = module.superModule;\nvar superTotal = base.total;\nbase.total = function (b) {\n    return superTotal(b) + base.total.length;\n};\nmodule.exports = base;";
        let parsed = parse(text);
        let script = parsed.script();
        assert_eq!(
            texts(text, &script.uses("total", |_| true)),
            ["1:total", "3:superTotal"]
        );
        assert_eq!(texts(text, &script.local_uses("total")), ["3:total"]);
    }

    #[test]
    fn local_uses_are_the_calls_of_the_exported_function() {
        let text = "function total() {}\nfunction other() { return total(); }\nfunction f(total) { return total; }\nmodule.exports = { total: total };";
        let parsed = parse(text);
        assert_eq!(
            texts(text, &parsed.script().local_uses("total")),
            ["1:total"]
        );
    }

    #[test]
    fn lists_the_names_a_file_exports_and_whether_it_inherits_the_rest() {
        let names = |text: &str| parse(text).script().exported_names();
        assert_eq!(
            names("function a() {}\nmodule.exports = { a: a, b() {}, c: function () {} };"),
            (vec!["a".to_string(), "b".into(), "c".into()], false)
        );
        assert_eq!(
            names(
                "var base = module.superModule;\nbase.d = function () {};\nmodule.exports = base;"
            ),
            (vec!["d".to_string()], true)
        );
        assert_eq!(
            names("var base = module.superModule;\nmodule.exports = Object.assign({}, base, { e: 1 });"),
            (vec!["e".to_string()], true)
        );
        assert_eq!(
            names("var helpers = {};\nhelpers.f = function () {};\nexports.g = 1;\nmodule.exports = helpers;"),
            (vec!["f".to_string()], false)
        );
    }

    #[test]
    fn reads_each_parameter_whole() {
        let text = "/** Doc. */\nfunction total(basket, b = [1, 2], { c, d }, ...rest) {}\nmodule.exports = { total: total };";
        let parsed = parse(text);
        let script = parsed.script();
        let Export::Defines(definition) = script.export("total") else {
            panic!("not defined");
        };
        let description = script.describe(&definition.sites[0], "total");
        assert_eq!(
            description.signature.as_deref(),
            Some("total(basket, b = [1, 2], { c, d }, ...rest)")
        );
        let names: Vec<Option<&str>> = description
            .parameters
            .iter()
            .map(|(_, name)| name.as_deref())
            .collect();
        assert_eq!(names, [Some("basket"), Some("b"), None, None]);
    }

    #[test]
    fn a_template_is_read_at_its_own_offsets() {
        let text = "<div class=\"é\">${require('*/x').total(a)}</div>\n<iscomment>${require('*/x').total()}</iscomment>\n<isscript>\n  var h = require('*/x'); h.total();\n</isscript>";
        let parsed = Parsed::new(Path::new("x.isml"), text).unwrap();
        let uses = parsed.script().uses("total", |_| true);
        assert_eq!(texts(text, &uses), ["0:total", "3:total"]);
    }
}
