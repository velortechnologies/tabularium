use tantivy::schema::{IndexRecordOption, Schema, TextFieldIndexing, TextOptions};
use tantivy::{Index, Term, doc};
fn numeric_id_options() -> tantivy::schema::NumericOptions {
    tantivy::schema::NumericOptions::default()
        .set_indexed()
        .set_fast()
        .set_stored()
}

fn raw_text_options() -> TextOptions {
    TextOptions::default().set_stored().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::Basic),
    )
}

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    b.add_u64_field("doc_id", numeric_id_options());
    b.add_text_field("dir_path", raw_text_options());
    let text_opts = TextOptions::default().set_stored().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    b.add_text_field("file_name", text_opts.clone());
    b.add_text_field("description", text_opts.clone());
    b.add_text_field("content", text_opts);
    b.build()
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    std::fs::create_dir_all(&path).unwrap();
    let schema = build_schema();
    let index = Index::create_in_dir(&path, schema.clone()).unwrap();
    let id = schema.get_field("doc_id").unwrap();
    let dir = schema.get_field("dir_path").unwrap();
    let name = schema.get_field("file_name").unwrap();
    let desc = schema.get_field("description").unwrap();
    let content = schema.get_field("content").unwrap();
    let mut writer = index.writer(50_000_000).unwrap();
    for (i, p, n, c) in [
        (1u64, "/one", "legacy.md", "legacytoken alpha"),
        (2, "/two", "other.md", "legacytoken beta"),
        (3, "/one/child", "removed.md", "deletedtoken"),
    ] {
        writer
            .add_document(doc!(id=>i,dir=>p,name=>n,desc=>"legacy description",content=>c))
            .unwrap();
        writer.commit().unwrap();
    }
    writer.delete_term(Term::from_field_u64(id, 3));
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
}
