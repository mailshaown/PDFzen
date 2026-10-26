//! Password protection and the PDFzen integrity seal.
//!
//! PDF encryption is written with the standard security handler (AES-256,
//! revision 6) from `pdfrum-crypt`.  The crate deliberately leaves the object
//! graph walk to its caller, so this module walks every indirect object and
//! enciphers its strings and stream data with the object identity the format
//! requires.
//!
//! A password-protected file also gets a small HMAC-SHA256 marker appended
//! after the PDF itself.  PDF readers ignore trailing comments; PDFzen checks
//! the marker when it opens a protected file and can therefore report whether
//! the bytes changed since the file was saved.  This is an application-level
//! tamper-evidence check, not a standard PDF digital signature.

use getrandom as _;
use hmac::{Hmac, KeyInit, Mac};
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};
use pdfrum_crypt::{CryptClass, Iv, KeyMaterial, Permissions, SecurityHandler, standard_r6};
use pdfrum_object::{
    Array as PdfrumArray, Dict as PdfrumDict, Name as PdfrumName, NoResolve, ObjRef,
    Object as PdfrumObject, PdfString, StringSyntax,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SEAL_PREFIX: &[u8] = b"%PDFzen-Seal: 1 ";
const SEAL_VERSION: &str = "1";

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityInput {
    pub user_password: String,
    #[serde(default)]
    pub owner_password: String,
    #[serde(default)]
    pub allow_print: bool,
    #[serde(default)]
    pub allow_copy: bool,
    #[serde(default)]
    pub allow_modify: bool,
    #[serde(default)]
    pub allow_annotate: bool,
    #[serde(default)]
    pub allow_assemble: bool,
}

impl SecurityInput {
    fn permissions(&self) -> Permissions {
        Permissions {
            print: self.allow_print,
            modify: self.allow_modify,
            copy: self.allow_copy,
            annotate: self.allow_annotate,
            fill_form: self.allow_annotate,
            extract: self.allow_copy,
            assemble: self.allow_assemble,
            print_high_quality: self.allow_print,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SealStatus {
    Missing,
    Valid,
    Invalid,
}

#[derive(Debug)]
pub struct OpenOutcome {
    pub data: Vec<u8>,
    pub encrypted: bool,
    pub seal: SealStatus,
}

/// Add an AES-256 `/Encrypt` dictionary to `doc` and encipher every indirect
/// string and stream.  The document is upgraded to PDF 2.0 because revision 6
/// is defined by ISO 32000-2.
pub fn encrypt_document(doc: &mut Document, input: &SecurityInput) -> Result<(), String> {
    if input.user_password.is_empty() {
        return Err("An open password is required for password protection.".into());
    }

    let key_material = KeyMaterial::from_os().map_err(|e| format!("random generator: {e:?}"))?;
    let (pdfrum_dict, handler) = standard_r6(
        input.user_password.as_bytes(),
        input.owner_password.as_bytes(),
        input.permissions(),
        true,
        &key_material,
    )
    .map_err(|e| format!("protect pdf: {e:?}"))?;

    let encrypt_id = doc.add_object(Object::Dictionary(pdfrum_dict_to_lopdf(&pdfrum_dict)));
    doc.trailer.set("Encrypt", Object::Reference(encrypt_id));
    doc.trailer.set(
        "ID",
        Object::Array(vec![
            hex_string(&random_bytes::<16>()?),
            hex_string(&random_bytes::<16>()?),
        ]),
    );
    doc.version = "2.0".to_string();

    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        if id == encrypt_id {
            continue;
        }
        if let Some(object) = doc.objects.get_mut(&id) {
            encrypt_object(object, id, &handler)?;
        }
    }
    Ok(())
}

/// Open a PDF, decrypting it when necessary.
///
/// `Err("PASSWORD_REQUIRED")` means the caller must ask for a password;
/// `Err("WRONG_PASSWORD")` means the supplied password was rejected.
pub fn open_pdf_bytes(bytes: &[u8], password: Option<&str>) -> Result<OpenOutcome, String> {
    let doc = Document::load_mem(bytes).map_err(|e| format!("load pdf: {e}"))?;
    if !doc.is_encrypted() {
        return Ok(OpenOutcome {
            data: bytes.to_vec(),
            encrypted: false,
            seal: SealStatus::Missing,
        });
    }

    let supplied = password.unwrap_or("");
    match decrypt_document(bytes, supplied) {
        Ok(data) => {
            let seal = verify_seal(bytes, supplied);
            Ok(OpenOutcome {
                data,
                encrypted: true,
                seal,
            })
        }
        Err(err) if err == "WRONG_PASSWORD" => {
            if password.is_none() {
                Err("PASSWORD_REQUIRED".into())
            } else {
                Err("WRONG_PASSWORD".into())
            }
        }
        Err(err) => Err(err),
    }
}

/// Append an HMAC-SHA256 marker to a finished encrypted PDF.  The key is
/// derived from the open password and a fresh random salt.
pub fn append_seal(mut bytes: Vec<u8>, password: &str) -> Result<Vec<u8>, String> {
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let salt = random_bytes::<16>()?;
    let key = seal_key(&salt, password);
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|e| format!("seal key: {e:?}"))?;
    mac.update(&bytes);
    let tag = mac.finalize().into_bytes();
    let line = format!(
        "%PDFzen-Seal: {SEAL_VERSION} {} {}\n",
        hex_encode(&salt),
        hex_encode(&tag)
    );
    bytes.extend_from_slice(line.as_bytes());
    Ok(bytes)
}

fn verify_seal(bytes: &[u8], password: &str) -> SealStatus {
    let Some(start) = find_seal_line(bytes) else {
        return SealStatus::Missing;
    };
    let end = bytes[start..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|offset| start + offset)
        .unwrap_or(bytes.len());
    let Ok(line) = std::str::from_utf8(&bytes[start..end]) else {
        return SealStatus::Invalid;
    };
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() != 4 || fields[0] != "%PDFzen-Seal:" || fields[1] != SEAL_VERSION {
        return SealStatus::Invalid;
    }
    let (Some(salt), Some(tag)) = (hex_decode(fields[2]), hex_decode(fields[3])) else {
        return SealStatus::Invalid;
    };
    let key = seal_key(&salt, password);
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&key) else {
        return SealStatus::Invalid;
    };
    mac.update(&bytes[..start]);
    if mac.verify_slice(&tag).is_ok() {
        SealStatus::Valid
    } else {
        SealStatus::Invalid
    }
}

fn find_seal_line(bytes: &[u8]) -> Option<usize> {
    let mut found = None;
    for (index, window) in bytes.windows(SEAL_PREFIX.len()).enumerate() {
        if window == SEAL_PREFIX && (index == 0 || bytes[index - 1] == b'\n') {
            found = Some(index);
        }
    }
    found
}

fn seal_key(salt: &[u8], password: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(password.as_bytes());
    hasher.finalize().into()
}

fn decrypt_document(bytes: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let mut doc = Document::load_mem(bytes).map_err(|e| format!("load pdf: {e}"))?;
    let encrypt_id = match doc.trailer.get(b"Encrypt") {
        Ok(Object::Reference(id)) => *id,
        _ => return Err("load pdf: missing /Encrypt dictionary".into()),
    };
    let encrypt_dict = doc
        .get_dictionary(encrypt_id)
        .map_err(|e| format!("read /Encrypt: {e}"))?
        .clone();
    let pdfrum_dict = lopdf_dict_to_pdfrum(&encrypt_dict, &doc, 0);
    let file_id = file_id(&doc);

    let handler =
        SecurityHandler::from_encrypt_dict(&pdfrum_dict, &file_id, password.as_bytes(), &NoResolve)
            .map_err(|e| {
                if matches!(e, pdfrum_crypt::Error::WrongPassword) {
                    "WRONG_PASSWORD".to_string()
                } else {
                    format!("decrypt pdf: {e:?}")
                }
            })?;

    let encrypt_metadata = handler.encrypt_metadata();
    let metadata_id = if encrypt_metadata {
        None
    } else {
        doc.catalog()
            .ok()
            .and_then(|catalog| catalog.get(b"Metadata").ok())
            .and_then(|object| object.as_reference().ok())
    };

    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        if id == encrypt_id || Some(id) == metadata_id {
            continue;
        }
        if let Some(object) = doc.objects.get_mut(&id) {
            decrypt_object(object, id, &handler)?;
        }
    }
    doc.trailer.remove(b"Encrypt");

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("write decrypted pdf: {e}"))?;
    Ok(out)
}

fn encrypt_object(
    object: &mut Object,
    id: ObjectId,
    handler: &SecurityHandler,
) -> Result<(), String> {
    match object {
        Object::String(data, format) => {
            let iv = Iv(random_bytes::<16>()?);
            *data = handler.encrypt(ObjRef::new(id.0, id.1), CryptClass::String, iv, data);
            *format = StringFormat::Hexadecimal;
        }
        Object::Array(items) => {
            for item in items {
                encrypt_object(item, id, handler)?;
            }
        }
        Object::Dictionary(dict) => {
            let signature = is_signature_dictionary(dict);
            for (key, value) in dict.iter_mut() {
                if signature && key == b"Contents" {
                    continue;
                }
                encrypt_object(value, id, handler)?;
            }
        }
        Object::Stream(stream) => {
            let signature = is_signature_dictionary(&stream.dict);
            for (key, value) in stream.dict.iter_mut() {
                if signature && key == b"Contents" {
                    continue;
                }
                encrypt_object(value, id, handler)?;
            }
            let iv = Iv(random_bytes::<16>()?);
            let content = handler.encrypt(
                ObjRef::new(id.0, id.1),
                CryptClass::Stream,
                iv,
                &stream.content,
            );
            stream.set_content(content);
        }
        _ => {}
    }
    Ok(())
}

fn decrypt_object(
    object: &mut Object,
    id: ObjectId,
    handler: &SecurityHandler,
) -> Result<(), String> {
    match object {
        Object::String(data, _) => {
            *data = handler.decrypt(ObjRef::new(id.0, id.1), CryptClass::String, data);
        }
        Object::Array(items) => {
            for item in items {
                decrypt_object(item, id, handler)?;
            }
        }
        Object::Dictionary(dict) => {
            let signature = is_signature_dictionary(dict);
            for (key, value) in dict.iter_mut() {
                if signature && key == b"Contents" {
                    continue;
                }
                decrypt_object(value, id, handler)?;
            }
        }
        Object::Stream(stream) => {
            let signature = is_signature_dictionary(&stream.dict);
            for (key, value) in stream.dict.iter_mut() {
                if signature && key == b"Contents" {
                    continue;
                }
                decrypt_object(value, id, handler)?;
            }
            let content =
                handler.decrypt(ObjRef::new(id.0, id.1), CryptClass::Stream, &stream.content);
            stream.set_content(content);
        }
        _ => {}
    }
    Ok(())
}

fn is_signature_dictionary(dict: &Dictionary) -> bool {
    match dict.get(b"Type") {
        Ok(Object::Name(name)) => return name == b"Sig",
        Ok(_) => return false,
        Err(_) => {}
    }
    matches!(dict.get(b"FT"), Ok(Object::Name(name)) if name == b"Sig")
}

fn file_id(doc: &Document) -> Vec<u8> {
    let Ok(Object::Array(ids)) = doc.trailer.get(b"ID") else {
        return Vec::new();
    };
    match ids.first() {
        Some(Object::String(bytes, _)) => bytes.clone(),
        _ => Vec::new(),
    }
}

fn pdfrum_dict_to_lopdf(dict: &PdfrumDict) -> Dictionary {
    let mut out = Dictionary::new();
    for (key, value) in dict.iter() {
        out.set(key.as_bytes().to_vec(), pdfrum_to_lopdf(value));
    }
    out
}

fn pdfrum_to_lopdf(object: &PdfrumObject) -> Object {
    match object {
        PdfrumObject::Null => Object::Null,
        PdfrumObject::Bool(value) => Object::Boolean(*value),
        PdfrumObject::Int(value) => Object::Integer(*value),
        PdfrumObject::Real(value) => Object::Real(*value),
        PdfrumObject::Str(value) => Object::String(
            value.as_bytes().to_vec(),
            if value.is_hex() {
                StringFormat::Hexadecimal
            } else {
                StringFormat::Literal
            },
        ),
        PdfrumObject::Name(value) => Object::Name(value.as_bytes().to_vec()),
        PdfrumObject::Array(values) => Object::Array(values.iter().map(pdfrum_to_lopdf).collect()),
        PdfrumObject::Dict(value) => Object::Dictionary(pdfrum_dict_to_lopdf(value)),
        PdfrumObject::Stream(_) | PdfrumObject::Ref(_) => Object::Null,
    }
}

fn lopdf_dict_to_pdfrum(dict: &Dictionary, doc: &Document, depth: usize) -> PdfrumDict {
    let mut out = PdfrumDict::new();
    for (key, value) in dict.iter() {
        out.insert(
            PdfrumName::new(key.clone()),
            lopdf_to_pdfrum(value, doc, depth + 1),
        );
    }
    out
}

fn lopdf_to_pdfrum(object: &Object, doc: &Document, depth: usize) -> PdfrumObject {
    if depth > 16 {
        return PdfrumObject::Null;
    }
    match object {
        Object::Null => PdfrumObject::Null,
        Object::Boolean(value) => PdfrumObject::Bool(*value),
        Object::Integer(value) => PdfrumObject::Int(*value),
        Object::Real(value) => PdfrumObject::Real(*value),
        Object::Name(value) => PdfrumObject::Name(PdfrumName::new(value.clone())),
        Object::String(value, format) => PdfrumObject::Str(PdfString::new(
            value.clone(),
            if *format == StringFormat::Hexadecimal {
                StringSyntax::Hex
            } else {
                StringSyntax::Literal
            },
        )),
        Object::Array(values) => PdfrumObject::Array(PdfrumArray::of(
            values
                .iter()
                .map(|value| lopdf_to_pdfrum(value, doc, depth + 1)),
        )),
        Object::Dictionary(value) => {
            PdfrumObject::Dict(lopdf_dict_to_pdfrum(value, doc, depth + 1))
        }
        Object::Stream(_) => PdfrumObject::Null,
        Object::Reference(id) => doc
            .get_object(*id)
            .map(|value| lopdf_to_pdfrum(value, doc, depth + 1))
            .unwrap_or(PdfrumObject::Null),
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| format!("random generator: {e}"))?;
    Ok(bytes)
}

fn hex_string(bytes: &[u8]) -> Object {
    Object::String(bytes.to_vec(), StringFormat::Hexadecimal)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let pair = std::str::from_utf8(&bytes[index..index + 2]).ok()?;
        out.push(u8::from_str_radix(pair, 16).ok()?);
        index += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(password: &str) -> SecurityInput {
        SecurityInput {
            user_password: password.to_string(),
            owner_password: String::new(),
            allow_print: true,
            allow_copy: true,
            allow_modify: false,
            allow_annotate: false,
            allow_assemble: false,
        }
    }

    #[test]
    fn encrypted_document_reopens_with_the_password_and_seal() {
        let mut doc = Document::load_mem(&crate::pdf::blank(200.0, 200.0).unwrap()).unwrap();
        encrypt_document(&mut doc, &options("hunter2")).unwrap();
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let sealed = append_seal(bytes, "hunter2").unwrap();

        let opened = open_pdf_bytes(&sealed, Some("hunter2")).unwrap();
        assert!(opened.encrypted);
        assert_eq!(opened.seal, SealStatus::Valid);
        assert_eq!(
            Document::load_mem(&opened.data).unwrap().get_pages().len(),
            1
        );
    }

    #[test]
    fn wrong_password_is_rejected_and_missing_password_is_requested() {
        let mut doc = Document::load_mem(&crate::pdf::blank(100.0, 100.0).unwrap()).unwrap();
        encrypt_document(&mut doc, &options("secret")).unwrap();
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();

        assert_eq!(
            open_pdf_bytes(&bytes, None).unwrap_err(),
            "PASSWORD_REQUIRED"
        );
        assert_eq!(
            open_pdf_bytes(&bytes, Some("wrong")).unwrap_err(),
            "WRONG_PASSWORD"
        );
    }

    #[test]
    fn seal_detects_byte_changes() {
        let bytes = append_seal(b"%PDF-1.5\n%%EOF\n".to_vec(), "secret").unwrap();
        assert_eq!(verify_seal(&bytes, "secret"), SealStatus::Valid);
        assert_eq!(verify_seal(&bytes, "wrong"), SealStatus::Invalid);
        let mut changed = bytes.clone();
        changed[9] ^= 1;
        assert_eq!(verify_seal(&changed, "secret"), SealStatus::Invalid);
    }
}
