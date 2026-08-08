use convert_case::{Case, Casing};
use tera::{Filter, Kwargs, State, Tera, TeraResult};

pub fn register_all(tera: &mut Tera) {
    tera.register_filter("pascal_case", PascalCaseFilter);
    tera.register_filter("camel_case", CamelCaseFilter);
    tera.register_filter("kebab_case", KebabCaseFilter);
    tera.register_filter("snake_case", SnakeCaseFilter);
    tera.register_filter("title_case", TitleCaseFilter);
    tera.register_filter("flat_case", FlatCaseFilter);
    tera.register_filter("cobol_case", CobolCaseFilter);
    tera.register_filter("train_case", TrainCaseFilter);
}

struct PascalCaseFilter;

impl Filter<String, TeraResult<String>> for PascalCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Pascal))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct CamelCaseFilter;

impl Filter<String, TeraResult<String>> for CamelCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Camel))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct KebabCaseFilter;

impl Filter<String, TeraResult<String>> for KebabCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Kebab))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct SnakeCaseFilter;

impl Filter<String, TeraResult<String>> for SnakeCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Snake))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct TitleCaseFilter;

impl Filter<String, TeraResult<String>> for TitleCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Title))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct FlatCaseFilter;

impl Filter<String, TeraResult<String>> for FlatCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Flat))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct CobolCaseFilter;

impl Filter<String, TeraResult<String>> for CobolCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Cobol))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

struct TrainCaseFilter;

impl Filter<String, TeraResult<String>> for TrainCaseFilter {
    fn call(&self, value: String, _kwargs: Kwargs, _state: &State) -> TeraResult<String> {
        Ok(value.to_case(Case::Train))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use tera::Context;

    use super::*;

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
            .render_str(input.as_str(), &context, false)
            .expect("String should be rendered");

        assert_eq!(expected, result)
    }
}
