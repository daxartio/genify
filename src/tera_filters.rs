use convert_case::{Case, Casing};
use std::{collections::HashMap, hash::BuildHasher};
use tera::{to_value, try_get_value, Result, Tera, Value};

pub fn register_all(tera: &mut Tera) {
    tera.register_filter("pascal_case", pascal_case);
    tera.register_filter("camel_case", camel_case);
    tera.register_filter("kebab_case", kebab_case);
    tera.register_filter("snake_case", snake_case);
    tera.register_filter("title_case", title_case);
    tera.register_filter("flat_case", flat_case);
    tera.register_filter("cobol_case", cobol_case);
    tera.register_filter("train_case", train_case);
}

fn pascal_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("pascal_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Pascal)).unwrap())
}

fn camel_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("camel_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Camel)).unwrap())
}

fn kebab_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("kebab_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Kebab)).unwrap())
}

fn snake_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("snake_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Snake)).unwrap())
}

fn title_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("title_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Title)).unwrap())
}

fn flat_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("flat_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Flat)).unwrap())
}

fn cobol_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("cobol_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Cobol)).unwrap())
}

fn train_case<S: BuildHasher>(value: &Value, _: &HashMap<String, Value, S>) -> Result<Value> {
    let s = try_get_value!("train_case", "value", String, value);
    Ok(to_value(s.to_case(Case::Train)).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tera::Context;

    #[test]
    fn test() {
        let mut tera = Tera::default();
        register_all(&mut tera);

        let context = Context::new();

        let input = [
            "{{ 'some text' | pascal_case }}",
            "{{ 'some text' | camel_case }}",
            "{{ 'some text' | kebab_case }}",
            "{{ 'some text' | snake_case }}",
            "{{ 'some text' | title_case }}",
            "{{ 'some text' | flat_case }}",
            "{{ 'some text' | cobol_case }}",
            "{{ 'some text' | train_case }}",
        ]
        .join("\n");
        let expected = [
            "SomeText",
            "someText",
            "some-text",
            "some_text",
            "Some Text",
            "sometext",
            "SOME-TEXT",
            "Some-Text",
        ]
        .join("\n");

        let result = tera
            .render_str(input.as_str(), &context)
            .expect("String should be rendered");

        assert_eq!(expected, result)
    }
}
