use std::rc::Rc;

use litparser_derive::LitParser;
use swc_common::sync::Lrc;
use swc_common::{Span, Spanned};
use swc_ecma_ast as ast;

use litparser::{report_and_continue, LitParser, ParseResult, Sp, ToParseErr};

use crate::parser::resourceparser::bind::{BindData, BindKind, ResourceOrPath};
use crate::parser::resourceparser::paths::PkgPath;
use crate::parser::resourceparser::resource_parser::ResourceParser;
use crate::parser::resourceparser::ResourceParseContext;
use crate::parser::resources::parseutil::{
    iter_references, resolve_object_for_bind_name, NamedClassResource, TrackedNames,
};
use crate::parser::resources::Resource;
use crate::parser::types::Object;

#[derive(Debug, Clone)]
pub struct CronJob {
    pub name: String,
    pub title: Option<String>,
    pub doc: Option<String>,
    pub schedule: CronJobSchedule,
    pub endpoint: Sp<Rc<Object>>,
}

#[derive(Debug, Clone)]
pub enum CronJobSchedule {
    Every(u32), // every N minutes
    Cron(CronExpr),
}

#[derive(Debug, Clone)]
pub struct CronExpr(pub String);

#[derive(Debug, LitParser)]
struct DecodedCronJobConfig {
    endpoint: ast::Expr,
    title: Option<String>,
    every: Option<Sp<std::time::Duration>>,
    schedule: Option<Sp<CronExpr>>,
}

pub const CRON_PARSER: ResourceParser = ResourceParser {
    name: "cron",
    interesting_pkgs: &[PkgPath("encore.dev/cron")],

    run: |pass| {
        let names = TrackedNames::new(&[("encore.dev/cron", "CronJob")]);

        let module = pass.module.clone();
        type Res = NamedClassResource<DecodedCronJobConfig>;
        for r in iter_references::<Res>(&module, &names) {
            let r = report_and_continue!(r);
            report_and_continue!(parse_cron_job(pass, r));
        }
    },
};

fn parse_cron_job(
    pass: &mut ResourceParseContext,
    r: NamedClassResource<DecodedCronJobConfig>,
) -> ParseResult<()> {
    let object = resolve_object_for_bind_name(pass.type_checker, pass.module.clone(), &r.bind_name);

    let endpoint = pass
        .type_checker
        .resolve_obj(pass.module.clone(), &r.config.endpoint)
        .ok_or(r.config.endpoint.parse_err("cannot resolve endpoint"))?;

    let schedule = r.config.parse_schedule(r.range.to_span())?;
    let resource = Resource::CronJob(Lrc::new(CronJob {
        name: r.resource_name.to_owned(),
        doc: r.doc_comment,
        title: r.config.title,
        endpoint: Sp::new(r.config.endpoint.span(), endpoint),
        schedule,
    }));
    pass.add_resource(resource.clone());
    pass.add_bind(BindData {
        range: r.range,
        resource: ResourceOrPath::Resource(resource),
        object,
        kind: BindKind::Create,
        ident: r.bind_name,
    });
    Ok(())
}

impl LitParser for CronExpr {
    fn parse_lit(input: &ast::Expr) -> ParseResult<Self> {
        match input {
            ast::Expr::Lit(ast::Lit::Str(str)) => {
                // Ensure the cron expression is valid
                let expr = str.value.as_ref();
                cron_parser::parse(expr, &chrono::Utc::now())
                    .map_err(|err| input.parse_err(err.to_string()))?;
                Ok(CronExpr(expr.to_string()))
            }
            _ => Err(input.parse_err("expected cron expression")),
        }
    }
}

impl DecodedCronJobConfig {
    fn parse_schedule(&self, def_span: Span) -> ParseResult<CronJobSchedule> {
        match (self.every, self.schedule.as_ref()) {
            (None, Some(schedule)) => Ok(CronJobSchedule::Cron(schedule.clone().take())),
            (Some(every), None) => {
                // TODO introduce more robust validation and error reporting here.
                let secs = every.as_secs();
                if secs % 60 != 0 {
                    return Err(every
                        .span()
                        .parse_err("`every` must be a multiple of 60 seconds"));
                }
                let mins = secs / 60;
                if mins > (24 * 60) {
                    return Err(every.span().parse_err("`every` must be at most 24 hours"));
                }
                Ok(CronJobSchedule::Every(mins as u32))
            }
            (None, None) => {
                Err(def_span.parse_err("expected either `every` or `schedule` to be set"))
            }
            (Some(_), Some(_)) => {
                Err(def_span.parse_err("expected either `every` or `schedule` to be set, not both"))
            }
        }
    }
}
