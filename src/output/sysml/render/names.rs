use heck::ToKebabCase;

pub(super) fn name(value: &str) -> String {
    const RESERVED: &str = "
        about abstract accept action actor after alias all allocate allocation analysis and as
        assert assign assume at attribute behavior bind binding by calc case class comment concern
        conjugate conjugated connect connection constraint crosses datatype decide def default
        defined dependency derived difference disjoining do doc done else end entry enum event
        exhibit exit expose extends false feature filter first flow for fork frame from hastype if
        implies import in include individual inout interaction interface intersect inv istype item
        join language library locale loop member merge merges message meta metadata nonunique not
        null objective occurrence of or ordered out package parallel part perform port portion
        private protected public readonly redefines ref references render rendering rep require
        requirement return satisfy send snapshot specializes stakeholder standard start state step
        subject subsets succession terminate that then this timeslice to transition true type until
        use variant variation verification verify via view viewpoint when where while xor
    ";

    if !value.is_empty()
        && !value.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.split_whitespace().any(|s| s == value)
    {
        value.into()
    } else {
        format!(
            "'{}'",
            value
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    }
}

pub(super) fn qualified(value: &str) -> String {
    value.split("::").map(name).collect::<Vec<_>>().join("::")
}

pub(super) fn filename(value: &str) -> String {
    value.to_kebab_case()
}

pub(crate) fn identifier(value: &str) -> String {
    let identifier: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();

    if name(&identifier) == identifier {
        identifier
    } else {
        format!("value_{identifier}")
    }
}
