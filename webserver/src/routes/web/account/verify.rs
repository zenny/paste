use crate::{
  config::Config,
  database::{
    DbConn,
    schema::{email_verifications, users},
    models::{
      email_verifications::EmailVerification,
      users::User,
    },
  },
  errors::*,
  i18n::prelude::*,
  models::id::EmailVerificationId,
  routes::web::{OptionalWebUser, Session},
  utils::HashedPassword,
};

use chrono::Utc;

use data_encoding::BASE64URL_NOPAD;

use diesel::prelude::*;

use rocket::{
  State,
  request::Form,
  response::Redirect,
};

use sidekiq::Client as SidekiqClient;

use sodiumoxide::randombytes;

#[post("/account/send_verification", format = "application/x-www-form-urlencoded", data = "<data>")]
pub fn resend(data: Form<Resend>, config: State<Config>, user: OptionalWebUser, mut sess: Session, conn: DbConn, sidekiq: State<SidekiqClient>, l10n: L10n) -> Result<Redirect> {
  let data = data.into_inner();

  if !sess.check_token(&data.anti_csrf_token) {
    sess.add_data("error", l10n.tr("error-csrf")?);
    return Ok(Redirect::to(uri!(super::index::get)));
  }

  let user = match user.into_inner() {
    Some(u) => u,
    None => return Ok(Redirect::to(uri!(crate::routes::web::auth::login::get))),
  };

  if user.email_verified() {
    sess.add_data("error", l10n.tr(("email-verify-error", "already-verified"))?);
    return Ok(Redirect::to(uri!(super::index::get)));
  }

  let ver: Option<EmailVerification> = email_verifications::table
    .filter(email_verifications::user_id.eq(*user.id())
      .and(email_verifications::email.eq(user.email())))
    .first(&*conn)
    .optional()?;

  let (mut ver, secret) = match ver {
    Some(v) => (v, None),
    None => {
      let (v, s) = user.create_email_verification(&conn, None)?;
      (v, Some(s))
    },
  };

  if !ver.can_send_again() {
    sess.add_data("error", l10n.tr(("email-verify-error", "resend-too-soon"))?);
    return Ok(Redirect::to(uri!(super::index::get)));
  }

  let secret = match secret {
    Some(s) => s,
    None => {
      let s = randombytes::randombytes(32);
      let key = HashedPassword::from(&s).into_string();

      ver.set_key(key);
      ver.update(&conn)?;

      s
    },
  };

  ver.update_last_sent(&conn, Utc::now().naive_utc())?;

  sidekiq.push(ver.job(&config, &user, &secret)?.into())?;

  sess.add_data("info", l10n.tr("email-verify-sent")?);
  Ok(Redirect::to(uri!(super::index::get)))
}

#[get("/account/verify?<data..>")]
pub fn get(data: Form<Verification>, mut sess: Session, conn: DbConn, l10n: L10n) -> Result<Redirect> {
  let key = match BASE64URL_NOPAD.decode(data.key.as_bytes()) {
    Ok(k) => k,
    Err(_) => {
      sess.add_data("error", l10n.tr(("email-verify-error", "invalid"))?);
      return Ok(Redirect::to(uri!(super::index::get)));
    },
  };

  let verification: Option<EmailVerification> = email_verifications::table
    .find(*data.id)
    .first(&*conn)
    .optional()?;

  let verification = match verification {
    Some(v) => v,
    None => {
      sess.add_data("error", l10n.tr(("email-verify-error", "invalid"))?);
      return Ok(Redirect::to(uri!(super::index::get)));
    },
  };

  if !verification.check(&key) {
    sess.add_data("error", l10n.tr(("email-verify-error", "invalid"))?);
    return Ok(Redirect::to(uri!(super::index::get)));
  }

  let mut user: User = match users::table
    .find(verification.user_id)
    .filter(users::email.eq(&verification.email))
    .first(&*conn)
    .optional()?
  {
    Some(u) => u,
    None => {
      sess.add_data("error", l10n.tr(("email-verify-error", "invalid"))?);
      return Ok(Redirect::to(uri!(super::index::get)));
    },
  };

  if user.email_verified() {
    sess.add_data("error", l10n.tr(("email-verify-error", "already-verified"))?);
    return Ok(Redirect::to(uri!(super::index::get)));
  }

  user.set_email_verified(true);
  user.update(&conn)?;

  diesel::delete(&verification).execute(&*conn)?;

  sess.add_data("info", l10n.tr("email-verify-success")?);
  Ok(Redirect::to(uri!(super::index::get)))
}

#[derive(Debug, FromForm)]
pub struct Verification {
  id: EmailVerificationId,
  key: String,
}

#[derive(Debug, FromForm)]
pub struct Resend {
  anti_csrf_token: String,
}
