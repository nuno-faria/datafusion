// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! Code to convert Arrow schemas and DataFusion logical plans to protocol buffer format, allowing
//! DataFusion logical plans to be serialized and transmitted between
//! processes.

use std::collections::HashMap;

use datafusion_common::{NullEquality, TableReference, UnnestOptions};
use datafusion_expr::dml::InsertOp;
use datafusion_expr::expr::{
    self, AggregateFunctionParams, Alias, Between, BinaryExpr, Cast, GroupingSet, InList,
    Like, Placeholder, ScalarFunction, Unnest,
};
use datafusion_expr::WriteOp;
use datafusion_expr::{
    logical_plan::PlanType, logical_plan::StringifiedPlan, Expr, JoinConstraint,
    JoinType, SortExpr, TryCast, WindowFrame, WindowFrameBound, WindowFrameUnits,
    WindowFunctionDefinition,
};

use crate::protobuf::RecursionUnnestOption;
use crate::protobuf::{
    self,
    plan_type::PlanTypeEnum::{
        AnalyzedLogicalPlan, FinalAnalyzedLogicalPlan, FinalLogicalPlan,
        FinalPhysicalPlan, FinalPhysicalPlanWithSchema, FinalPhysicalPlanWithStats,
        InitialLogicalPlan, InitialPhysicalPlan, InitialPhysicalPlanWithSchema,
        InitialPhysicalPlanWithStats, OptimizedLogicalPlan, OptimizedPhysicalPlan,
        PhysicalPlanError,
    },
    AnalyzedLogicalPlanType, CubeNode, EmptyMessage, GroupingSetNode, LogicalExprList,
    OptimizedLogicalPlanType, OptimizedPhysicalPlanType, PlaceholderNode, RollupNode,
    ToProtoError as Error,
};

use super::LogicalExtensionCodec;

impl From<&UnnestOptions> for protobuf::UnnestOptions {
    fn from(opts: &UnnestOptions) -> Self {
        Self {
            preserve_nulls: opts.preserve_nulls,
            recursions: opts
                .recursions
                .iter()
                .map(|r| RecursionUnnestOption {
                    input_column: Some((&r.input_column).into()),
                    output_column: Some((&r.output_column).into()),
                    depth: r.depth as u32,
                })
                .collect(),
        }
    }
}

impl From<&StringifiedPlan> for protobuf::StringifiedPlan {
    fn from(stringified_plan: &StringifiedPlan) -> Self {
        Self {
            plan_type: match stringified_plan.clone().plan_type {
                PlanType::InitialLogicalPlan => Some(protobuf::PlanType {
                    plan_type_enum: Some(InitialLogicalPlan(EmptyMessage {})),
                }),
                PlanType::AnalyzedLogicalPlan { analyzer_name } => {
                    Some(protobuf::PlanType {
                        plan_type_enum: Some(AnalyzedLogicalPlan(
                            AnalyzedLogicalPlanType { analyzer_name },
                        )),
                    })
                }
                PlanType::FinalAnalyzedLogicalPlan => Some(protobuf::PlanType {
                    plan_type_enum: Some(FinalAnalyzedLogicalPlan(EmptyMessage {})),
                }),
                PlanType::OptimizedLogicalPlan { optimizer_name } => {
                    Some(protobuf::PlanType {
                        plan_type_enum: Some(OptimizedLogicalPlan(
                            OptimizedLogicalPlanType { optimizer_name },
                        )),
                    })
                }
                PlanType::FinalLogicalPlan => Some(protobuf::PlanType {
                    plan_type_enum: Some(FinalLogicalPlan(EmptyMessage {})),
                }),
                PlanType::InitialPhysicalPlan => Some(protobuf::PlanType {
                    plan_type_enum: Some(InitialPhysicalPlan(EmptyMessage {})),
                }),
                PlanType::OptimizedPhysicalPlan { optimizer_name } => {
                    Some(protobuf::PlanType {
                        plan_type_enum: Some(OptimizedPhysicalPlan(
                            OptimizedPhysicalPlanType { optimizer_name },
                        )),
                    })
                }
                PlanType::FinalPhysicalPlan => Some(protobuf::PlanType {
                    plan_type_enum: Some(FinalPhysicalPlan(EmptyMessage {})),
                }),
                PlanType::InitialPhysicalPlanWithStats => Some(protobuf::PlanType {
                    plan_type_enum: Some(InitialPhysicalPlanWithStats(EmptyMessage {})),
                }),
                PlanType::InitialPhysicalPlanWithSchema => Some(protobuf::PlanType {
                    plan_type_enum: Some(InitialPhysicalPlanWithSchema(EmptyMessage {})),
                }),
                PlanType::FinalPhysicalPlanWithStats => Some(protobuf::PlanType {
                    plan_type_enum: Some(FinalPhysicalPlanWithStats(EmptyMessage {})),
                }),
                PlanType::FinalPhysicalPlanWithSchema => Some(protobuf::PlanType {
                    plan_type_enum: Some(FinalPhysicalPlanWithSchema(EmptyMessage {})),
                }),
                PlanType::PhysicalPlanError => Some(protobuf::PlanType {
                    plan_type_enum: Some(PhysicalPlanError(EmptyMessage {})),
                }),
            },
            plan: stringified_plan.plan.to_string(),
        }
    }
}

impl From<WindowFrameUnits> for protobuf::WindowFrameUnits {
    fn from(units: WindowFrameUnits) -> Self {
        match units {
            WindowFrameUnits::Rows => Self::Rows,
            WindowFrameUnits::Range => Self::Range,
            WindowFrameUnits::Groups => Self::Groups,
        }
    }
}

impl TryFrom<&WindowFrameBound> for protobuf::WindowFrameBound {
    type Error = Error;

    fn try_from(bound: &WindowFrameBound) -> Result<Self, Self::Error> {
        Ok(match bound {
            WindowFrameBound::CurrentRow => Self {
                window_frame_bound_type: protobuf::WindowFrameBoundType::CurrentRow
                    .into(),
                bound_value: None,
            },
            WindowFrameBound::Preceding(v) => Self {
                window_frame_bound_type: protobuf::WindowFrameBoundType::Preceding.into(),
                bound_value: Some(v.try_into()?),
            },
            WindowFrameBound::Following(v) => Self {
                window_frame_bound_type: protobuf::WindowFrameBoundType::Following.into(),
                bound_value: Some(v.try_into()?),
            },
        })
    }
}

impl TryFrom<&WindowFrame> for protobuf::WindowFrame {
    type Error = Error;

    fn try_from(window: &WindowFrame) -> Result<Self, Self::Error> {
        Ok(Self {
            window_frame_units: protobuf::WindowFrameUnits::from(window.units).into(),
            start_bound: Some((&window.start_bound).try_into()?),
            end_bound: Some(protobuf::window_frame::EndBound::Bound(
                (&window.end_bound).try_into()?,
            )),
        })
    }
}

pub fn serialize_exprs<'a, I>(
    exprs: I,
    codec: &dyn LogicalExtensionCodec,
) -> Result<Vec<protobuf::LogicalExprNode>, Error>
where
    I: IntoIterator<Item = &'a Expr>,
{
    exprs
        .into_iter()
        .map(|expr| serialize_expr(expr, codec))
        .collect::<Result<Vec<_>, Error>>()
}

pub fn serialize_expr(
    expr: &Expr,
    codec: &dyn LogicalExtensionCodec,
) -> Result<protobuf::LogicalExprNode, Error> {
    use protobuf::logical_expr_node::ExprType;

    let expr_node = match expr {
        Expr::Column(c) => protobuf::LogicalExprNode {
            expr_type: Some(ExprType::Column(c.into())),
        },
        Expr::Alias(Alias {
            expr,
            relation,
            name,
            metadata,
        }) => {
            let alias = Box::new(protobuf::AliasNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                relation: relation
                    .to_owned()
                    .map(|r| vec![r.into()])
                    .unwrap_or(vec![]),
                alias: name.to_owned(),
                metadata: metadata
                    .as_ref()
                    .map(|m| m.to_hashmap())
                    .unwrap_or(HashMap::new()),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Alias(alias)),
            }
        }
        Expr::Literal(value, _) => {
            let pb_value: protobuf::ScalarValue = value.try_into()?;
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Literal(pb_value)),
            }
        }
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            // Try to linerize a nested binary expression tree of the same operator
            // into a flat vector of expressions.
            let mut exprs = vec![right.as_ref()];
            let mut current_expr = left.as_ref();
            while let Expr::BinaryExpr(BinaryExpr {
                left,
                op: current_op,
                right,
            }) = current_expr
            {
                if current_op == op {
                    exprs.push(right.as_ref());
                    current_expr = left.as_ref();
                } else {
                    break;
                }
            }
            exprs.push(current_expr);

            let binary_expr = protobuf::BinaryExprNode {
                // We need to reverse exprs since operands are expected to be
                // linearized from left innermost to right outermost (but while
                // traversing the chain we do the exact opposite).
                operands: serialize_exprs(exprs.into_iter().rev(), codec)?,
                op: format!("{op:?}"),
            };
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::BinaryExpr(binary_expr)),
            }
        }
        Expr::Like(Like {
            negated,
            expr,
            pattern,
            escape_char,
            case_insensitive,
        }) => {
            if *case_insensitive {
                let pb = Box::new(protobuf::ILikeNode {
                    negated: *negated,
                    expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                    pattern: Some(Box::new(serialize_expr(pattern.as_ref(), codec)?)),
                    escape_char: escape_char.map(|ch| ch.to_string()).unwrap_or_default(),
                });

                protobuf::LogicalExprNode {
                    expr_type: Some(ExprType::Ilike(pb)),
                }
            } else {
                let pb = Box::new(protobuf::LikeNode {
                    negated: *negated,
                    expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                    pattern: Some(Box::new(serialize_expr(pattern.as_ref(), codec)?)),
                    escape_char: escape_char.map(|ch| ch.to_string()).unwrap_or_default(),
                });

                protobuf::LogicalExprNode {
                    expr_type: Some(ExprType::Like(pb)),
                }
            }
        }
        Expr::SimilarTo(Like {
            negated,
            expr,
            pattern,
            escape_char,
            case_insensitive: _,
        }) => {
            let pb = Box::new(protobuf::SimilarToNode {
                negated: *negated,
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                pattern: Some(Box::new(serialize_expr(pattern.as_ref(), codec)?)),
                escape_char: escape_char.map(|ch| ch.to_string()).unwrap_or_default(),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::SimilarTo(pb)),
            }
        }
        Expr::WindowFunction(window_fun) => {
            let expr::WindowFunction {
                ref fun,
                params:
                    expr::WindowFunctionParams {
                        ref args,
                        ref partition_by,
                        ref order_by,
                        ref window_frame,
                        // TODO: support null treatment in proto
                        null_treatment: _,
                        distinct: _,
                    },
            } = window_fun.as_ref();
            let mut buf = Vec::new();
            let window_function = match fun {
                WindowFunctionDefinition::AggregateUDF(aggr_udf) => {
                    let _ = codec.try_encode_udaf(aggr_udf, &mut buf);
                    protobuf::window_expr_node::WindowFunction::Udaf(
                        aggr_udf.name().to_string(),
                    )
                }
                WindowFunctionDefinition::WindowUDF(window_udf) => {
                    let _ = codec.try_encode_udwf(window_udf, &mut buf);
                    protobuf::window_expr_node::WindowFunction::Udwf(
                        window_udf.name().to_string(),
                    )
                }
            };
            let fun_definition = (!buf.is_empty()).then_some(buf);
            let partition_by = serialize_exprs(partition_by, codec)?;
            let order_by = serialize_sorts(order_by, codec)?;

            let window_frame: Option<protobuf::WindowFrame> =
                Some(window_frame.try_into()?);
            let window_expr = protobuf::WindowExprNode {
                exprs: serialize_exprs(args, codec)?,
                window_function: Some(window_function),
                partition_by,
                order_by,
                window_frame,
                fun_definition,
            };
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::WindowExpr(window_expr)),
            }
        }
        Expr::AggregateFunction(expr::AggregateFunction {
            ref func,
            params:
                AggregateFunctionParams {
                    ref args,
                    ref distinct,
                    ref filter,
                    ref order_by,
                    null_treatment: _,
                },
        }) => {
            let mut buf = Vec::new();
            let _ = codec.try_encode_udaf(func, &mut buf);
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::AggregateUdfExpr(Box::new(
                    protobuf::AggregateUdfExprNode {
                        fun_name: func.name().to_string(),
                        args: serialize_exprs(args, codec)?,
                        distinct: *distinct,
                        filter: match filter {
                            Some(e) => Some(Box::new(serialize_expr(e.as_ref(), codec)?)),
                            None => None,
                        },
                        order_by: serialize_sorts(order_by, codec)?,
                        fun_definition: (!buf.is_empty()).then_some(buf),
                    },
                ))),
            }
        }

        Expr::ScalarVariable(_, _) => {
            return Err(Error::General(
                "Proto serialization error: Scalar Variable not supported".to_string(),
            ))
        }
        Expr::ScalarFunction(ScalarFunction { func, args }) => {
            let mut buf = Vec::new();
            let _ = codec.try_encode_udf(func, &mut buf);
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::ScalarUdfExpr(protobuf::ScalarUdfExprNode {
                    fun_name: func.name().to_string(),
                    fun_definition: (!buf.is_empty()).then_some(buf),
                    args: serialize_exprs(args, codec)?,
                })),
            }
        }
        Expr::Not(expr) => {
            let expr = Box::new(protobuf::Not {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::NotExpr(expr)),
            }
        }
        Expr::IsNull(expr) => {
            let expr = Box::new(protobuf::IsNull {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsNullExpr(expr)),
            }
        }
        Expr::IsNotNull(expr) => {
            let expr = Box::new(protobuf::IsNotNull {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsNotNullExpr(expr)),
            }
        }
        Expr::IsTrue(expr) => {
            let expr = Box::new(protobuf::IsTrue {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsTrue(expr)),
            }
        }
        Expr::IsFalse(expr) => {
            let expr = Box::new(protobuf::IsFalse {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsFalse(expr)),
            }
        }
        Expr::IsUnknown(expr) => {
            let expr = Box::new(protobuf::IsUnknown {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsUnknown(expr)),
            }
        }
        Expr::IsNotTrue(expr) => {
            let expr = Box::new(protobuf::IsNotTrue {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsNotTrue(expr)),
            }
        }
        Expr::IsNotFalse(expr) => {
            let expr = Box::new(protobuf::IsNotFalse {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsNotFalse(expr)),
            }
        }
        Expr::IsNotUnknown(expr) => {
            let expr = Box::new(protobuf::IsNotUnknown {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::IsNotUnknown(expr)),
            }
        }
        Expr::Between(Between {
            expr,
            negated,
            low,
            high,
        }) => {
            let expr = Box::new(protobuf::BetweenNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                negated: *negated,
                low: Some(Box::new(serialize_expr(low.as_ref(), codec)?)),
                high: Some(Box::new(serialize_expr(high.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Between(expr)),
            }
        }
        Expr::Case(case) => {
            let when_then_expr = case
                .when_then_expr
                .iter()
                .map(|(w, t)| {
                    Ok(protobuf::WhenThen {
                        when_expr: Some(serialize_expr(w.as_ref(), codec)?),
                        then_expr: Some(serialize_expr(t.as_ref(), codec)?),
                    })
                })
                .collect::<Result<Vec<protobuf::WhenThen>, Error>>()?;
            let expr = Box::new(protobuf::CaseNode {
                expr: match &case.expr {
                    Some(e) => Some(Box::new(serialize_expr(e.as_ref(), codec)?)),
                    None => None,
                },
                when_then_expr,
                else_expr: match &case.else_expr {
                    Some(e) => Some(Box::new(serialize_expr(e.as_ref(), codec)?)),
                    None => None,
                },
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Case(expr)),
            }
        }
        Expr::Cast(Cast { expr, data_type }) => {
            let expr = Box::new(protobuf::CastNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                arrow_type: Some(data_type.try_into()?),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Cast(expr)),
            }
        }
        Expr::TryCast(TryCast { expr, data_type }) => {
            let expr = Box::new(protobuf::TryCastNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                arrow_type: Some(data_type.try_into()?),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::TryCast(expr)),
            }
        }
        Expr::Negative(expr) => {
            let expr = Box::new(protobuf::NegativeNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Negative(expr)),
            }
        }
        Expr::Unnest(Unnest { expr }) => {
            let expr = protobuf::Unnest {
                exprs: vec![serialize_expr(expr.as_ref(), codec)?],
            };
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Unnest(expr)),
            }
        }
        Expr::InList(InList {
            expr,
            list,
            negated,
        }) => {
            let expr = Box::new(protobuf::InListNode {
                expr: Some(Box::new(serialize_expr(expr.as_ref(), codec)?)),
                list: serialize_exprs(list, codec)?,
                negated: *negated,
            });
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::InList(expr)),
            }
        }
        #[expect(deprecated)]
        Expr::Wildcard { qualifier, .. } => protobuf::LogicalExprNode {
            expr_type: Some(ExprType::Wildcard(protobuf::Wildcard {
                qualifier: qualifier.to_owned().map(|x| x.into()),
            })),
        },
        Expr::ScalarSubquery(_)
        | Expr::InSubquery(_)
        | Expr::Exists { .. }
        | Expr::OuterReferenceColumn { .. } => {
            // we would need to add logical plan operators to datafusion.proto to support this
            // see discussion in https://github.com/apache/datafusion/issues/2565
            return Err(Error::General("Proto serialization error: Expr::ScalarSubquery(_) | Expr::InSubquery(_) | Expr::Exists { .. } | Exp:OuterReferenceColumn not supported".to_string()));
        }
        Expr::GroupingSet(GroupingSet::Cube(exprs)) => protobuf::LogicalExprNode {
            expr_type: Some(ExprType::Cube(CubeNode {
                expr: serialize_exprs(exprs, codec)?,
            })),
        },
        Expr::GroupingSet(GroupingSet::Rollup(exprs)) => protobuf::LogicalExprNode {
            expr_type: Some(ExprType::Rollup(RollupNode {
                expr: serialize_exprs(exprs, codec)?,
            })),
        },
        Expr::GroupingSet(GroupingSet::GroupingSets(exprs)) => {
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::GroupingSet(GroupingSetNode {
                    expr: exprs
                        .iter()
                        .map(|expr_list| {
                            Ok(LogicalExprList {
                                expr: serialize_exprs(expr_list, codec)?,
                            })
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                })),
            }
        }
        Expr::Placeholder(Placeholder { id, data_type }) => {
            let data_type = match data_type {
                Some(data_type) => Some(data_type.try_into()?),
                None => None,
            };
            protobuf::LogicalExprNode {
                expr_type: Some(ExprType::Placeholder(PlaceholderNode {
                    id: id.clone(),
                    data_type,
                })),
            }
        }
    };

    Ok(expr_node)
}

pub fn serialize_sorts<'a, I>(
    sorts: I,
    codec: &dyn LogicalExtensionCodec,
) -> Result<Vec<protobuf::SortExprNode>, Error>
where
    I: IntoIterator<Item = &'a SortExpr>,
{
    sorts
        .into_iter()
        .map(|sort| {
            let SortExpr {
                expr,
                asc,
                nulls_first,
            } = sort;
            Ok(protobuf::SortExprNode {
                expr: Some(serialize_expr(expr, codec)?),
                asc: *asc,
                nulls_first: *nulls_first,
            })
        })
        .collect::<Result<Vec<_>, Error>>()
}

impl From<TableReference> for protobuf::TableReference {
    fn from(t: TableReference) -> Self {
        use protobuf::table_reference::TableReferenceEnum;
        let table_reference_enum = match t {
            TableReference::Bare { table } => {
                TableReferenceEnum::Bare(protobuf::BareTableReference {
                    table: table.to_string(),
                })
            }
            TableReference::Partial { schema, table } => {
                TableReferenceEnum::Partial(protobuf::PartialTableReference {
                    schema: schema.to_string(),
                    table: table.to_string(),
                })
            }
            TableReference::Full {
                catalog,
                schema,
                table,
            } => TableReferenceEnum::Full(protobuf::FullTableReference {
                catalog: catalog.to_string(),
                schema: schema.to_string(),
                table: table.to_string(),
            }),
        };

        protobuf::TableReference {
            table_reference_enum: Some(table_reference_enum),
        }
    }
}

impl From<JoinType> for protobuf::JoinType {
    fn from(t: JoinType) -> Self {
        match t {
            JoinType::Inner => protobuf::JoinType::Inner,
            JoinType::Left => protobuf::JoinType::Left,
            JoinType::Right => protobuf::JoinType::Right,
            JoinType::Full => protobuf::JoinType::Full,
            JoinType::LeftSemi => protobuf::JoinType::Leftsemi,
            JoinType::RightSemi => protobuf::JoinType::Rightsemi,
            JoinType::LeftAnti => protobuf::JoinType::Leftanti,
            JoinType::RightAnti => protobuf::JoinType::Rightanti,
            JoinType::LeftMark => protobuf::JoinType::Leftmark,
            JoinType::RightMark => protobuf::JoinType::Rightmark,
        }
    }
}

impl From<JoinConstraint> for protobuf::JoinConstraint {
    fn from(t: JoinConstraint) -> Self {
        match t {
            JoinConstraint::On => protobuf::JoinConstraint::On,
            JoinConstraint::Using => protobuf::JoinConstraint::Using,
        }
    }
}

impl From<NullEquality> for protobuf::NullEquality {
    fn from(t: NullEquality) -> Self {
        match t {
            NullEquality::NullEqualsNothing => protobuf::NullEquality::NullEqualsNothing,
            NullEquality::NullEqualsNull => protobuf::NullEquality::NullEqualsNull,
        }
    }
}

impl From<&WriteOp> for protobuf::dml_node::Type {
    fn from(t: &WriteOp) -> Self {
        match t {
            WriteOp::Insert(InsertOp::Append) => protobuf::dml_node::Type::InsertAppend,
            WriteOp::Insert(InsertOp::Overwrite) => {
                protobuf::dml_node::Type::InsertOverwrite
            }
            WriteOp::Insert(InsertOp::Replace) => protobuf::dml_node::Type::InsertReplace,
            WriteOp::Delete => protobuf::dml_node::Type::Delete,
            WriteOp::Update => protobuf::dml_node::Type::Update,
            WriteOp::Ctas => protobuf::dml_node::Type::Ctas,
        }
    }
}
