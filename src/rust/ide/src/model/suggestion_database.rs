//! The module contains all structures for representing suggestions and their database.
pub mod entry;
pub mod example;

use crate::prelude::*;

use crate::double_representation::module::QualifiedName;
use crate::double_representation::tp;
use crate::model::module::MethodId;
use crate::model::suggestion_database::entry::Kind;
use crate::notification;

use data::text::TextLocation;
use enso_protocol::language_server;
use enso_protocol::language_server::{SuggestionId, SuggestionsDatabaseModification};
use flo_stream::Subscriber;
use language_server::types::SuggestionDatabaseUpdatesEvent;
use language_server::types::SuggestionsDatabaseVersion;

pub use entry::Entry;
pub use example::Example;
use crate::controller::searcher::action::Suggestion;


// ==============
// === Errors ===
// ==============

#[allow(missing_docs)]
#[derive(Debug,Clone,Copy,Eq,Fail,PartialEq)]
#[fail(display = "The suggestion with id {} has not been found in the database.", _0)]
pub struct NoSuchEntry(pub SuggestionId);



// ====================
// === Notification ===
// ====================

/// Notification about change in a suggestion database,
#[derive(Clone,Copy,Debug,PartialEq)]
pub enum Notification {
    /// The database has been updated.
    Updated
}

#[derive(Debug)]
struct DataStore {
    storage: HashMap<entry::Id,Rc<Entry>>,
}

/// Indicates that updating a suggestion failed.
#[derive(Debug)]
pub enum UpdateError {
    /// There was no entry with the given ID in the data store.
    InvalidEntry(entry::Id),
    /// There was an issue applying one of the modification. For every issue an error is returned.
    UpdateFailures(Vec<failure::Error>)
}

pub struct ModuleDocumentation {
    module : Rc<Entry>,
    atoms  : Vec<AtomDocs>,
    others : Vec<Rc<Entry>>,
}

pub struct AtomDocs {
    atom     : Rc<Entry>,
    methods : Vec<Rc<Entry>>,
}

fn in_doc_container(s:String) -> String {
    format!("<div class=\"doc\" style=\"font-size:13p;\">{}</div>",s)
}

fn in_atoms_section_container(s:String) -> String {
    if s.is_empty() {
        s
    } else {
        format!("<div class=\"separator\">Atoms</div>{}",s)
    }
}

fn in_methods_section_container(s:String) -> String {
    if s.is_empty() {
        s
    } else {
        format!("<div class=\"separator\">Methods</div>{}",s)
    }
}


const NO_DOCS_PLACEHOLDER: &str = "<p style=\"color: #a3a6a9;\">No documentation available</p>";
const NO_ATOMS_PLACEHOLDER: &str = "<p style=\"color: #a3a6a9;\">No atoms available</p>";
const NO_METHODS_PLACEHOLDER: &str = "<p style=\"color: #a3a6a9;\">No methods available</p>";
impl From<AtomDocs> for Documentation {
    fn from(docs: AtomDocs) -> Self {
        let mut output = format!("<p>{} - Atom</p>", docs.atom.name);
        output.extend(docs.atom.documentation_html.clone().unwrap_or(NO_DOCS_PLACEHOLDER.to_string()).chars());
        output.extend("<p>Atom Methods</p>".chars());
        for doc in &docs.methods {
            output.extend(format!("<hr><p>{}</p>", doc.name).chars());
            output.extend(doc.documentation_html.clone().unwrap_or(NO_METHODS_PLACEHOLDER.to_string()).chars());
        }
        in_doc_container(output)
    }
}



impl From<ModuleDocumentation> for Documentation {
    fn from(docs: ModuleDocumentation) -> Self {
        let mut output = format!("<p>{} - Module</p>", docs.module.name);
        output.extend(docs.module.documentation_html.clone().unwrap_or(NO_DOCS_PLACEHOLDER.to_string()).chars());
        // output.extend("<p>Module Atoms</p>".chars());
        let atom_doc:String = docs.atoms.into_iter().map_into::<Documentation>().collect();
        output.extend(in_atoms_section_container(atom_doc).chars());
        // output.extend("<p>Module Methods</p>".chars());
        let methods:String = docs.others.into_iter().map(|entry| {
            let heading = &entry.name;
            let doc     = entry.documentation_html.clone().unwrap_or(NO_DOCS_PLACEHOLDER.to_string());
            format!("<p>{}</p>{}",heading,doc)
        }).collect();
        output.extend(in_methods_section_container(methods).chars());
        in_doc_container(output)
    }
}


impl DataStore {
    fn new() -> DataStore {
        let storage = default();
        DataStore{storage}
    }

    fn from_entries(entries:impl IntoIterator<Item=(SuggestionId, Entry)>) -> DataStore {
        let mut data_store = Self::new();
        let entries = entries.into_iter().map(|(id,entry)| (id,Rc::new(entry)));
        data_store.storage.extend(entries);
        data_store
    }

    fn insert_entries<'a>(&mut self, entries:impl IntoIterator<Item=(&'a SuggestionId,&'a Entry)>) {
        entries.into_iter().for_each(|item| self.insert_entry(item))
    }

    fn insert_entry(&mut self, entry:(&SuggestionId,&Entry)) {
        self.storage.insert(*entry.0,Rc::new(entry.1.clone()));
    }

    fn remove_entry(&mut self, id:SuggestionId) -> Option<Rc<Entry>> {
        self.storage.remove(&id)
    }

    fn update_entry(&mut self, id: entry::Id, modification:SuggestionsDatabaseModification) -> Result<(),UpdateError>{
        if let Some(old_entry) = self.storage.get_mut(&id) {
            let entry  = Rc::make_mut(old_entry);
            let errors = entry.apply_modifications(modification);
            if errors.is_empty() {
                Ok(())
            } else {
                Err(UpdateError::UpdateFailures(errors))
            }
        } else {
            Err(UpdateError::InvalidEntry(id))
        }
    }

    fn get_entry(&self, id: entry::Id) -> Option<Rc<Entry>> {
        self.storage.get(&id).cloned()
    }

    fn get_method(&self, id:MethodId) -> Option<Rc<Entry>>{
        self.storage.values().find(|entry| entry.method_id().contains(&id)).cloned()
    }

    fn get_entry_by_name_and_location(&self, name:impl Str, module:&QualifiedName, location:TextLocation) -> Vec<Rc<Entry>>{
        self.storage.values().filter(|entry| {
            entry.matches_name(name.as_ref()) && entry.is_visible_at(module,location)
        }).cloned().collect()
    }

    fn get_locals_by_name_and_location(&self, name:impl Str, module:&QualifiedName, location:TextLocation) -> Vec<Rc<Entry>>{
        self.storage.values().filter(|entry| {
            let is_local = entry.kind == Kind::Function || entry.kind == Kind::Local;
            is_local && entry.matches_name(name.as_ref()) && entry.is_visible_at(module,location)
        }).cloned().collect()
    }

    fn get_module_method(&self, name:impl Str, module:&QualifiedName) ->Option<Rc<Entry>> {
        self.storage.values().find(|entry| {
            let is_method             = entry.kind == Kind::Method;
            let is_defined_for_module = entry.has_self_type(module);
            is_method && is_defined_for_module && entry.matches_name(name.as_ref())
        }).cloned()
    }

    fn get_module_methods(&self, module:&QualifiedName) -> Vec<Rc<Entry>> {
        self.storage.values().filter(|entry| {
            let is_method             = entry.kind == Kind::Method;
            let is_defined_for_module = entry.has_self_type(module);
            is_method && is_defined_for_module
        }).cloned().collect()
    }

    fn get_module_atoms(&self, module:&QualifiedName) -> Vec<Rc<Entry>> {
        self.storage.values().filter(|entry| {
            let is_method             = entry.kind == Kind::Atom;
            let is_defined_for_module = entry.module == *module;
            is_method && is_defined_for_module
        }).cloned().collect()
    }

    fn get_module(&self, module:&QualifiedName) -> Option<Rc<Entry>> {
        self.storage.values().find(|entry| {
            let is_method             = entry.kind == Kind::Module;
            let is_defined_for_module = entry.module == *module;
            is_method && is_defined_for_module
        }).cloned()
    }

    fn get_atom(&self, name:&tp::QualifiedName) -> Option<Rc<Entry>> {
        self.storage.values().find(|entry| {
            let is_method     = entry.kind == Kind::Atom;
            let matches_name = entry.qualified_name() == *name;
            is_method && matches_name
        }).cloned()
    }

    fn get_methods_for_type(&self, tp:&tp::QualifiedName) -> Vec<Rc<Entry>> {
        self.storage.values().filter(|entry| {
            let is_method             = entry.kind == Kind::Method;
            let is_defined_for_type   = entry.has_self_type(tp);
            is_method && is_defined_for_type
        }).cloned().collect()
    }
}

// ================
// === Database ===
// ================

/// The Suggestion Database
///
/// This is database of possible suggestions in Searcher. To achieve best performance, some
/// often-called Language Server methods returns the list of keys of this database instead of the
/// whole entries. Additionally the suggestions contains information about functions and their
/// argument names and types.
#[derive(Debug)]
pub struct SuggestionDatabase {
    logger        : Logger,
    entries       : RefCell<DataStore>,
    examples      : RefCell<Vec<Rc<Example>>>,
    version       : Cell<SuggestionsDatabaseVersion>,
    notifications : notification::Publisher<Notification>,
}

impl SuggestionDatabase {
       /// Create a database with no entries.
    pub fn new_empty(logger:impl AnyLogger) -> Self {
        let logger        = Logger::new_sub(logger,"SuggestionDatabase");
        let entries       = RefCell::new(DataStore::new());
        let examples      = default();
        let version       = default();
        let notifications = default();
        Self {logger,entries,examples,version,notifications}
    }


    /// Create a database filled with entries provided by the given iterator.
    pub fn new_from_entries<'a>
    (logger:impl AnyLogger, entries:impl IntoIterator<Item=(&'a SuggestionId,&'a Entry)>) -> Self {
        let ret     = Self::new_empty(logger);
        // let entries = entries.into_iter().map(|(id,entry)| (*id,Rc::new(entry.clone())));
        ret.entries.borrow_mut().insert_entries(entries);
        ret
    }

    /// Create a new database which will take its initial content from the Language Server.
    pub async fn create_synchronized
    (language_server:&language_server::Connection) -> FallibleResult<Self> {
        let response = language_server.client.get_suggestions_database().await?;
        Ok(Self::from_ls_response(response))
    }

    /// Create a new database model from response received from the Language Server.
    fn from_ls_response(response:language_server::response::GetSuggestionDatabase) -> Self {
        let logger      = Logger::new("SuggestionDatabase");

        let ls_entries =  response.entries.into_iter().filter_map(|ls_entry| {
            let id = ls_entry.id;
            match Entry::from_ls_entry(ls_entry.suggestion) {
                Ok(entry) => { Some((id, entry)) },
                Err(err)  => { error!(logger,"Discarded invalid entry {id}: {err}"); None },
            }
        });
        let entries = DataStore::from_entries(ls_entries);
        // let mut entries = HashMap::new();
        // for ls_entry in response.entries {
        //     let id = ls_entry.id;
        //     match Entry::from_ls_entry(ls_entry.suggestion) {
        //         Ok(entry) => { entries.insert(id, Rc::new(entry)); },
        //         Err(err)  => { error!(logger,"Discarded invalid entry {id}: {err}"); },
        //     }
        // }
        //TODO[ao]: This is a temporary solution. Eventually, we should gather examples from the
        //          available modules documentation. (https://github.com/enso-org/ide/issues/1011)
        let examples = example::EXAMPLES.iter().cloned().map(Rc::new).collect_vec();
        Self {
            logger,
            entries       : RefCell::new(entries),
            examples      : RefCell::new(examples),
            version       : Cell::new(response.current_version),
            notifications : default()
        }
    }

    /// Subscribe for notifications about changes in the database.
    pub fn subscribe(&self) -> Subscriber<Notification> {
        self.notifications.subscribe()
    }

    /// Get suggestion entry by id.
    pub fn lookup(&self, id:entry::Id) -> Result<Rc<Entry>,NoSuchEntry> {
        self.entries.borrow().get_entry(id).ok_or(NoSuchEntry(id))
    }

    /// Apply the update event to the database.
    pub fn apply_update_event(&self, event:SuggestionDatabaseUpdatesEvent) {
        for update in event.updates {
            let mut entries = self.entries.borrow_mut();
            match update {
                entry::Update::Add {id,suggestion} => match suggestion.try_into() {
                    Ok(entry) => { entries.insert_entry((&id,&entry));                       },
                    Err(err)  => { error!(self.logger, "Discarding update for {id}: {err}") },
                },
                entry::Update::Remove {id} => {
                    let removed = entries.remove_entry(id);
                    if removed.is_none() {
                        error!(self.logger, "Received Remove event for nonexistent id: {id}");
                    }
                },
                entry::Update::Modify
                    {id,modification,..} => {
                    if let Err(err) = entries.update_entry(id,*modification) {
                        error!(self.logger, || format!("Suggestion entry update failed: {:?}", err));
                    }

                }
            };
        }
        self.version.set(event.current_version);
        self.notifications.notify(Notification::Updated);
    }


    /// Look up given id in the suggestion database and if it is a known method obtain a pointer to
    /// it.
    pub fn lookup_method_ptr
    (&self, id:SuggestionId) -> FallibleResult<language_server::MethodPointer> {
        let entry = self.lookup(id)?;
        language_server::MethodPointer::try_from(entry.as_ref())
    }

    /// Search the database for an entry of method identified by given id.
    pub fn lookup_method(&self, id:MethodId) -> Option<Rc<Entry>> {
        self.entries.borrow().get_method(id)
    }

    /// Search the database for entries with given name and visible at given location in module.
    pub fn lookup_by_name_and_location
    (&self, name:impl Str, module:&QualifiedName, location:TextLocation) -> Vec<Rc<Entry>> {
        self.entries.borrow().get_entry_by_name_and_location(name,module,location)
    }

    /// Search the database for Local or Function entries with given name and visible at given
    /// location in module.
    pub fn lookup_locals_by_name_and_location
    (&self, name:impl Str, module:&QualifiedName, location:TextLocation) -> Vec<Rc<Entry>> {
        self.entries.borrow().get_locals_by_name_and_location(name,module,location)
    }

    /// Search the database for Method entry with given name and defined for given module.
    pub fn lookup_module_method
    (&self, name:impl Str, module:&QualifiedName) -> Option<Rc<Entry>> {
        self.entries.borrow().get_module_method(name,module)
    }

    /// An iterator over all examples gathered from suggestions.
    ///
    /// If the database was modified during iteration, the iterator does not panic, but may return
    /// unpredictable result (a mix of old and new values).
    pub fn iterate_examples(&self) -> impl Iterator<Item=Rc<Example>> + '_ {
        let indices = 0..self.examples.borrow().len();
        indices.filter_map(move |i| self.examples.borrow().get(i).cloned())
    }

    /// Put the entry to the database. Using this function likely breaks the synchronization between
    /// Language Server and IDE, and should be used only in tests.
    #[cfg(test)]
    pub fn put_entry(&self, id:entry::Id, entry:Entry) {
        self.entries.borrow_mut().insert_entry((&id,&entry))
    }

    fn get_atom_docs(&self, tp:&tp::QualifiedName) -> Option<AtomDocs> {
        let atom = self.entries.borrow().get_atom(tp)?;
        let methods = self.entries.borrow().get_methods_for_type(tp);
        Some(AtomDocs{atom,methods})
    }

    pub fn get_module_doc(&self, module:&QualifiedName) -> Option<ModuleDocumentation> {
        let module_entry = self.entries.borrow().get_module(module)?;
        let module_atom_entries = self.entries.borrow().get_module_atoms(module);
        let atom_types = module_atom_entries.iter().filter_map(|entry| entry.self_type.clone());
        let atom_docs = atom_types.filter_map(|atom_type| self.get_atom_docs(&atom_type)).collect();
        let others = self.entries.borrow().get_module_methods(module);
        Some(ModuleDocumentation {module:module_entry,atoms:atom_docs,others})
    }

    pub fn get_documentation(&self, id:entry::Id) -> Option<Documentation> {
        let entry = self.lookup(id).ok()?;
        self.get_documentation_for_entry(&entry)

    }

    pub fn get_documentation_for_entry(&self, entry:&Entry) -> Option<Documentation> {
        DEBUG!("{entry:#?}");
        let docs = match entry.kind {
            Kind::Atom   => {  Some(self.get_atom_docs(&entry.qualified_name())?.into()) }
            Kind::Module => {  Some(self.get_module_doc(&entry.module)?.into())}
            _            => entry.documentation_html.clone()
        };
        match docs {
            Some(s) if s.is_empty() => None,
            _                       => docs
        }
    }

    pub fn get_documentation_for_suggestion(&self, suggestion:&Suggestion) -> Option<Documentation> {
        match suggestion {
            Suggestion::FromDatabase(entry)   => self.get_documentation_for_entry(entry),
            Suggestion::Hardcoded(suggestion) => suggestion.documentation_html.map_ref(|doc| doc.to_string()),
        }
    }
}


pub type Documentation = String;

impl From<language_server::response::GetSuggestionDatabase> for SuggestionDatabase {
    fn from(database:language_server::response::GetSuggestionDatabase) -> Self {
        Self::from_ls_response(database)
    }
}



// =============
// === Tests ===
// =============

#[cfg(test)]
mod test {
    use super::*;

    use crate::executor::test_utils::TestWithLocalPoolExecutor;
    use crate::model::suggestion_database::entry::Scope;

    use enso_data::text::TextLocation;
    use enso_protocol::language_server::{SuggestionsDatabaseEntry, FieldUpdate, SuggestionsDatabaseModification};
    use enso_protocol::language_server::SuggestionArgumentUpdate;
    use enso_protocol::language_server::SuggestionEntryScope;
    use enso_protocol::language_server::Position;
    use enso_protocol::language_server::SuggestionEntryArgument;
    use utils::test::stream::StreamTestExt;
    use wasm_bindgen_test::wasm_bindgen_test_configure;



    wasm_bindgen_test_configure!(run_in_browser);



    #[test]
    fn initialize_database() {
        // Empty db
        let response = language_server::response::GetSuggestionDatabase {
            entries         : vec![],
            current_version : 123
        };
        let db = SuggestionDatabase::from_ls_response(response);
        assert!(db.entries.borrow().is_empty());
        assert_eq!(db.version.get()    , 123);

        // Non-empty db
        let entry = language_server::types::SuggestionEntry::Atom {
            name               : "TextAtom".to_string(),
            module             : "TestProject.TestModule".to_string(),
            arguments          : vec![],
            return_type        : "TestAtom".to_string(),
            documentation      : None,
            documentation_html : None,
            external_id        : None,
        };
        let db_entry = SuggestionsDatabaseEntry {id:12, suggestion:entry};
        let response = language_server::response::GetSuggestionDatabase {
            entries         : vec![db_entry],
            current_version : 456
        };
        let db = SuggestionDatabase::from_ls_response(response);
        assert_eq!(db.entries.borrow().len(), 1);
        assert_eq!(*db.lookup(12).unwrap().name, "TextAtom".to_string());
        assert_eq!(db.version.get(), 456);
    }

    //TODO[ao] this test should be split between various cases of applying modification to single
    //  entry and here only for testing whole database.
    #[test]
    fn applying_update() {
        let mut fixture = TestWithLocalPoolExecutor::set_up();
        let entry1 = language_server::types::SuggestionEntry::Atom {
            name               : "Entry1".to_owned(),
            module             : "TestProject.TestModule".to_owned(),
            arguments          : vec![],
            return_type        : "TestAtom".to_owned(),
            documentation      : None,
            documentation_html : None,
            external_id        : None,
        };
        let entry2 = language_server::types::SuggestionEntry::Atom {
            name               : "Entry2".to_owned(),
            module             : "TestProject.TestModule".to_owned(),
            arguments          : vec![],
            return_type        : "TestAtom".to_owned(),
            documentation      : None,
            documentation_html : None,
            external_id        : None,
        };
        let new_entry2 = language_server::types::SuggestionEntry::Atom {
            name               : "NewEntry2".to_owned(),
            module             : "TestProject.TestModule".to_owned(),
            arguments          : vec![],
            return_type        : "TestAtom".to_owned(),
            documentation      : None,
            documentation_html : None,
            external_id        : None,
        };
        let arg1 = SuggestionEntryArgument {
            name          : "Argument1".to_owned(),
            repr_type     : "Number".to_owned(),
            is_suspended  : false,
            has_default   : false,
            default_value : None
        };
        let arg2 = SuggestionEntryArgument {
            name          : "Argument2".to_owned(),
            repr_type     : "TestAtom".to_owned(),
            is_suspended  : true,
            has_default   : false,
            default_value : None
        };
        let arg3 = SuggestionEntryArgument {
            name          : "Argument3".to_owned(),
            repr_type     : "Number".to_owned(),
            is_suspended  : false,
            has_default   : true,
            default_value : Some("13".to_owned())
        };
        let entry3 = language_server::types::SuggestionEntry::Function {
            external_id : None,
            name        : "entry3".to_string(),
            module      : "TestProject.TestModule".to_string(),
            arguments   : vec![arg1,arg2,arg3],
            return_type : "".to_string(),
            scope       : SuggestionEntryScope {
                start : Position { line:1, character:2 },
                end   : Position { line:2, character:4 }
            }
        };

        let db_entry1        = SuggestionsDatabaseEntry {id:1, suggestion:entry1};
        let db_entry2        = SuggestionsDatabaseEntry {id:2, suggestion:entry2};
        let db_entry3        = SuggestionsDatabaseEntry {id:3, suggestion:entry3};
        let initial_response = language_server::response::GetSuggestionDatabase {
            entries         : vec![db_entry1,db_entry2,db_entry3],
            current_version : 1,
        };
        let db            = SuggestionDatabase::from_ls_response(initial_response);
        let mut notifications = db.subscribe().boxed_local();
        notifications.expect_pending();

        // Remove
        let remove_update = entry::Update::Remove {id:2};
        let update        = SuggestionDatabaseUpdatesEvent {
            updates         : vec![remove_update],
            current_version : 2
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        assert_eq!(db.lookup(2), Err(NoSuchEntry(2)));
        assert_eq!(db.version.get(), 2);

        // Add
        let add_update = entry::Update::Add {id:2, suggestion:new_entry2};
        let update     = SuggestionDatabaseUpdatesEvent {
            updates         : vec![add_update],
            current_version : 3,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(2).unwrap().name, "NewEntry2");
        assert_eq!(db.version.get(), 3);

        // Empty modify
        let modify_update = entry::Update::Modify {
            id            : 1,
            external_id   : None,
            modification  : Box::new(SuggestionsDatabaseModification {
                arguments          : vec![],
                module             : None,
                self_type          : None,
                return_type        : None,
                documentation      : None,
                documentation_html : None,
                scope              : None
            }),
        };
        let update = SuggestionDatabaseUpdatesEvent {
            updates         : vec![modify_update],
            current_version : 4,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(1).unwrap().arguments         , vec![]);
        assert_eq!(db.lookup(1).unwrap().return_type       , "TestAtom");
        assert_eq!(db.lookup(1).unwrap().documentation_html, None);
        assert!(matches!(db.lookup(1).unwrap().scope, Scope::Everywhere));
        assert_eq!(db.version.get(), 4);

        // Modify with some invalid fields
        let modify_update = entry::Update::Modify {
            id          : 1,
            external_id : None,
            modification : Box::new(SuggestionsDatabaseModification {
                // Invalid: the entry does not have any arguments.
                arguments:vec![SuggestionArgumentUpdate::Remove {index:0}],
                // Valid.
                return_type:Some(FieldUpdate::set("TestAtom2".to_owned())),
                // Valid.
                documentation:Some(FieldUpdate::set("Blah blah".to_owned())),
                // Valid.
                documentation_html:Some(FieldUpdate::set("<p>Blah blah</p>".to_owned())),
                // Invalid: atoms does not have any scope.
                scope:Some(FieldUpdate::set(SuggestionEntryScope {
                    start : Position {line:4, character:10},
                    end   : Position {line:8, character:12}
                })),
                module:None,
                self_type:None,
            }),
        };
        let update = SuggestionDatabaseUpdatesEvent {
            updates         : vec![modify_update],
            current_version : 5,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(1).unwrap().arguments         , vec![]);
        assert_eq!(db.lookup(1).unwrap().return_type       , "TestAtom2");
        assert_eq!(db.lookup(1).unwrap().documentation_html, Some("<p>Blah blah</p>".to_owned()));
        assert!(matches!(db.lookup(1).unwrap().scope, Scope::Everywhere));
        assert_eq!(db.version.get(), 5);

        // Modify Argument and Scope
        let modify_update = entry::Update::Modify {
            id            : 3,
            external_id   : None,
            modification  : Box::new(SuggestionsDatabaseModification {
                arguments     : vec![SuggestionArgumentUpdate::Modify {
                    index         : 2,
                    name          : Some(FieldUpdate::set("NewArg".to_owned())),
                    repr_type     : Some(FieldUpdate::set("TestAtom".to_owned())),
                    is_suspended  : Some(FieldUpdate::set(true)),
                    has_default   : Some(FieldUpdate::set(false)),
                    default_value : Some(FieldUpdate::remove()),
                }],
                return_type        : None,
                documentation      : None,
                documentation_html : None,
                scope              : Some(FieldUpdate::set(SuggestionEntryScope {
                    start : Position { line: 1, character: 5 },
                    end   : Position { line: 3, character: 0 }
                })),
                self_type : None,
                module    : None,
            }),
        };
        let update = SuggestionDatabaseUpdatesEvent {
            updates         : vec![modify_update],
            current_version : 6,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(3).unwrap().arguments.len(), 3);
        assert_eq!(db.lookup(3).unwrap().arguments[2].name, "NewArg");
        assert_eq!(db.lookup(3).unwrap().arguments[2].repr_type, "TestAtom");
        assert!   (db.lookup(3).unwrap().arguments[2].is_suspended);
        assert_eq!(db.lookup(3).unwrap().arguments[2].default_value, None);
        let range = TextLocation {line:1, column:5}..=TextLocation {line:3, column:0};
        assert_eq!(db.lookup(3).unwrap().scope, Scope::InModule{range});
        assert_eq!(db.version.get(), 6);

        // Add Argument
        let new_argument = SuggestionEntryArgument {
            name          : "NewArg2".to_string(),
            repr_type     : "Number".to_string(),
            is_suspended  : false,
            has_default   : false,
            default_value : None
        };
        let add_arg_update = entry::Update::Modify {
            id            : 3,
            external_id   : None,
            modification  : Box::new(SuggestionsDatabaseModification {
                arguments     : vec![SuggestionArgumentUpdate::Add {index:2, argument:new_argument}],
                return_type        : None,
                documentation      : None,
                documentation_html : None,
                scope              : None,
                self_type          : None,
                module             : None,
            }),
        };
        let update = SuggestionDatabaseUpdatesEvent {
            updates         : vec![add_arg_update],
            current_version : 7,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(3).unwrap().arguments.len(), 4);
        assert_eq!(db.lookup(3).unwrap().arguments[2].name, "NewArg2");
        assert_eq!(db.version.get(), 7);

        // Remove Argument
        let remove_arg_update = entry::Update::Modify {
            id            : 3,
            external_id   : None,
            modification  : Box::new(SuggestionsDatabaseModification {
                arguments     : vec![SuggestionArgumentUpdate::Remove {index:2}],
                return_type        : None,
                documentation      : None,
                documentation_html : None,
                scope              : None,
                self_type          : None,
                module             : None,
            }),
        };
        let update = SuggestionDatabaseUpdatesEvent {
            updates         : vec![remove_arg_update],
            current_version : 8,
        };
        db.apply_update_event(update);
        fixture.run_until_stalled();
        assert_eq!(notifications.expect_next(),Notification::Updated);
        notifications.expect_pending();
        assert_eq!(db.lookup(3).unwrap().arguments.len(), 3);
        assert_eq!(db.lookup(3).unwrap().arguments[2].name, "NewArg");
        assert_eq!(db.version.get(), 8);
    }
}
